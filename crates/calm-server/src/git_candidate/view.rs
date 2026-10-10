//! Pure derivations of the Planner read surface (`neige_task_ls.candidate`, D8): the effective
//! delivery state of one attempt from its rows (D2 derivation table) and the binding of one
//! task's current attempt (D8 first-match table). Inputs are rows; the only Operations read are the
//! attempt's gate ops (whether one exists, and the worker's runs, #2464 D6).

use calm_types::git_candidate::DeliveryFailureCode;
use calm_types::task_recovery::TASK_CHILD_TRACK_ROUTE;
use serde::Serialize;

use super::candidate::{CandidateRow, candidate_for_attempt_tx};
use super::delivery::{DeliveryRow, DeliverySettled, delivery_latest_for_attempt_tx};
use super::verification::{VerificationView, gate_runs_view, verification_state};
use crate::error::{CalmError, Result};
use crate::model::{Task, TaskAccess, TaskKind, TaskStatus};
use crate::operation::Tx;
use crate::operation::gate_ops::{GateOpKind, gate_op_outcome_tx, gate_ops_of_attempt_tx};
use crate::operation::task_gate_run::finalize::terminal_result;
use crate::operation::task_verify_adapter::{TASK_VERIFY_KIND, TaskGateResult, gate_attempt_key};
use crate::operation::workspace_lease::facts::{
    WorkerWorktreeFacts, latest_workspace_lease_for_attempt_tx,
};
use crate::operation::workspace_lease::{DeliveryPolicy, WorkspaceLease};

/// The `integrity.mismatches` entries the delivery derivation can name.
pub(crate) const MISMATCH_DELIVERY_ROW_MISSING: &str = "delivery_row_missing";
/// A `candidate` settlement without its candidate row, or a candidate row on a `failed`
/// settlement: both are impossible by construction (one transaction writes both) and are
/// reported as inconsistent rather than read as anything else.
pub(crate) const MISMATCH_CANDIDATE_ROW_MISSING: &str = "candidate_row_missing";
pub(crate) const MISMATCH_CANDIDATE_WITH_FAILED_SETTLEMENT: &str =
    "candidate_with_failed_settlement";

/// The failure facts the Planner reads from the delivery row (`failure_code`, `failure_reason`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct DeliveryFailure {
    pub code: DeliveryFailureCode,
    pub reason: String,
}

/// `delivery.state` — the seven-value effective state of one attempt's delivery.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum DeliveryState {
    /// The worker is still running; no delivery fact exists (no `delivery_id` is minted).
    NotReported,
    /// The attempt ended (`failed`) without ever reporting; it will never have a candidate.
    EndedWithoutDelivery {
        attempt_status: TaskStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        status_detail: Option<String>,
    },
    /// Rows contradict an invariant the kernel writes transactionally; never read as anything else.
    Inconsistent { mismatches: Vec<&'static str> },
    /// The row exists and is not settled (op missing, in flight, or terminal but unsettled).
    Pending { delivery_id: String, ordinal: i64 },
    /// The candidate OID differs from the base.
    Committed {
        delivery_id: String,
        ordinal: i64,
        candidate_id: String,
        commit_sha: String,
        r#ref: String,
        base_is_ancestor: bool,
    },
    /// The candidate OID equals the base — only OID equality, never "the script did not commit".
    NoChange {
        delivery_id: String,
        ordinal: i64,
        candidate_id: String,
        commit_sha: String,
        r#ref: String,
        base_is_ancestor: bool,
    },
    /// The latest delivery failed; no candidate exists and none will (a gated attempt still
    /// verifying failed with it, `delivery-failed`).
    Failed {
        delivery_id: String,
        ordinal: i64,
        failure: DeliveryFailure,
    },
}

/// D2 derivation table, one arm per row, total over `tasks.status × rows`. `pending` and
/// `canceled` attempts have no lease and are answered by [`candidate_binding`] before this is
/// reached; here they read as `not_reported` (no delivery fact exists for them either).
pub(crate) fn delivery_state(
    task_status: TaskStatus,
    status_detail: Option<&str>,
    delivery: Option<&DeliveryRow>,
    candidate: Option<&CandidateRow>,
) -> DeliveryState {
    let Some(delivery) = delivery else {
        return match task_status {
            TaskStatus::Pending
            | TaskStatus::Canceled
            | TaskStatus::Dispatched
            | TaskStatus::Running => DeliveryState::NotReported,
            TaskStatus::Failed => DeliveryState::EndedWithoutDelivery {
                attempt_status: task_status,
                status_detail: status_detail.map(str::to_string),
            },
            TaskStatus::Verifying | TaskStatus::Done => DeliveryState::Inconsistent {
                mismatches: vec![MISMATCH_DELIVERY_ROW_MISSING],
            },
        };
    };
    let delivery_id = delivery.delivery_id.clone();
    let ordinal = delivery.ordinal;
    match (&delivery.settlement, candidate) {
        (None, _) => DeliveryState::Pending {
            delivery_id,
            ordinal,
        },
        (Some(DeliverySettled::Candidate { .. }), Some(candidate)) => {
            let fields = (
                delivery_id,
                ordinal,
                candidate.candidate_id.clone(),
                candidate.commit_sha.clone(),
                candidate.ref_name.clone(),
                candidate.base_is_ancestor,
            );
            let (delivery_id, ordinal, candidate_id, commit_sha, r#ref, base_is_ancestor) = fields;
            if candidate.commit_sha == candidate.base_sha {
                DeliveryState::NoChange {
                    delivery_id,
                    ordinal,
                    candidate_id,
                    commit_sha,
                    r#ref,
                    base_is_ancestor,
                }
            } else {
                DeliveryState::Committed {
                    delivery_id,
                    ordinal,
                    candidate_id,
                    commit_sha,
                    r#ref,
                    base_is_ancestor,
                }
            }
        }
        (Some(DeliverySettled::Candidate { .. }), None) => DeliveryState::Inconsistent {
            mismatches: vec![MISMATCH_CANDIDATE_ROW_MISSING],
        },
        (Some(DeliverySettled::Failed { code, reason, .. }), None) => DeliveryState::Failed {
            delivery_id,
            ordinal,
            failure: DeliveryFailure {
                code: *code,
                reason: reason.clone(),
            },
        },
        (Some(DeliverySettled::Failed { .. }), Some(_)) => DeliveryState::Inconsistent {
            mismatches: vec![MISMATCH_CANDIDATE_WITH_FAILED_SETTLEMENT],
        },
    }
}

/// Why a task's current attempt has no candidate binding at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NoBindingReason {
    Terminal,
    ChildTrack,
    /// #1917: a read-only task delivers nothing.
    ReadOnly,
    NoLease,
}

/// Why a leased attempt is not bound: the lease predates kernel delivery.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UnboundReason {
    LegacyLease,
}

/// The `workspace` facts shown beside a binding: the worktree-facts subset the read surface shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct CandidateWorkspace {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub lease_state: String,
}

impl CandidateWorkspace {
    fn from_facts(facts: &WorkerWorktreeFacts) -> Self {
        CandidateWorkspace {
            path: facts.path.clone(),
            branch: facts.branch.clone(),
            lease_state: facts.state.clone(),
        }
    }
}

/// `candidate.binding` — D8's three shapes. `Bound` carries the delivery and verification views
/// inline (`clippy::large_enum_variant`): one value per read-surface entry, serialized once.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "binding", rename_all = "snake_case")]
pub(crate) enum CandidateBinding {
    None {
        reason: NoBindingReason,
    },
    Unbound {
        reason: UnboundReason,
        workspace: CandidateWorkspace,
    },
    Bound {
        producer_attempt_id: String,
        base_sha: String,
        workspace: CandidateWorkspace,
        delivery: DeliveryState,
        verification: VerificationView,
    },
}

impl CandidateBinding {
    /// The base `candidate.upstream` is measured from: `base_sha` of every
    /// bound candidate, whatever its lease's `base_source` ([`super::staleness`],
    /// computed after the read transaction); `None` for an unbound or
    /// unminted binding, which records no base.
    pub(crate) fn measured_base(&self) -> Option<&str> {
        match self {
            CandidateBinding::Bound { base_sha, .. } => Some(base_sha),
            _ => None,
        }
    }
}

/// The two derivations a kernel lease carries beside its binding (D2 `delivery`, D8 `verification`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BoundFacts {
    pub delivery: DeliveryState,
    pub verification: VerificationView,
}

/// D8 first-match table. `facts` and `bound` accompany `lease`: a lease row implies a facts row
/// (same table) and a kernel lease implies the caller derived the delivery and verification
/// states; a caller that breaks either contract gets an `Err`, never a guessed binding.
pub(crate) fn candidate_binding(
    task: &Task,
    lease: Option<&WorkspaceLease>,
    facts: Option<&WorkerWorktreeFacts>,
    bound: Option<BoundFacts>,
) -> Result<CandidateBinding> {
    if task.kind == TaskKind::Terminal {
        return Ok(CandidateBinding::None {
            reason: NoBindingReason::Terminal,
        });
    }
    if task.spawn == TASK_CHILD_TRACK_ROUTE {
        return Ok(CandidateBinding::None {
            reason: NoBindingReason::ChildTrack,
        });
    }
    if task.access == TaskAccess::ReadOnly {
        return Ok(CandidateBinding::None {
            reason: NoBindingReason::ReadOnly,
        });
    }
    let Some(lease) = lease else {
        return Ok(CandidateBinding::None {
            reason: NoBindingReason::NoLease,
        });
    };
    let Some(facts) = facts else {
        return Err(CalmError::Internal(format!(
            "task {}: lease {} has no worktree facts",
            task.id, lease.lease_id
        )));
    };
    let workspace = CandidateWorkspace::from_facts(facts);
    match (lease.delivery_policy, lease.base.as_ref(), bound) {
        (None, _, _) => Ok(CandidateBinding::Unbound {
            reason: UnboundReason::LegacyLease,
            workspace,
        }),
        (Some(DeliveryPolicy::Kernel), Some(base), Some(bound)) => Ok(CandidateBinding::Bound {
            producer_attempt_id: task.id.clone(),
            base_sha: base.base_sha.clone(),
            workspace,
            delivery: bound.delivery,
            verification: bound.verification,
        }),
        (Some(DeliveryPolicy::Kernel), None, _) => Err(CalmError::Internal(format!(
            "task {}: kernel lease {} has no base",
            task.id, lease.lease_id
        ))),
        (Some(DeliveryPolicy::Kernel), Some(_), None) => Err(CalmError::Internal(format!(
            "task {}: kernel lease {} without a derived delivery state",
            task.id, lease.lease_id
        ))),
    }
}

/// `neige_task_ls.candidate` for one current attempt: the attempt's lease row (the same latest
/// row `facts` was derived from), the attempt's latest delivery row and candidate
/// row for a kernel lease, then the pure derivations. `facts` is the entry's `worktree`
/// facts, read by the caller for the same attempt.
pub(crate) async fn candidate_view_tx(
    tx: &mut Tx<'_>,
    task: &Task,
    facts: Option<&WorkerWorktreeFacts>,
) -> Result<CandidateBinding> {
    let lease = latest_workspace_lease_for_attempt_tx(tx, &task.id).await?;
    let bound = match lease.as_ref() {
        Some(lease) if lease.delivery_policy == Some(DeliveryPolicy::Kernel) => {
            let delivery = delivery_latest_for_attempt_tx(tx, &task.id).await?;
            let candidate = candidate_for_attempt_tx(tx, &task.id).await?;
            Some(BoundFacts {
                delivery: delivery_state(
                    task.status,
                    task.status_detail.as_deref(),
                    delivery.as_ref(),
                    candidate.as_ref(),
                ),
                verification: verification_view_tx(tx, task).await?,
            })
        }
        _ => None,
    };
    candidate_binding(task, lease.as_ref(), facts, bound)
}

/// The D8 `verification` inputs of one row: its persisted verdict, and whether a task-verify
/// Operation exists at the row's `gate_attempt` or the next one (the op that bumped the row, or
/// one submitted and not yet through `prepare_tx`).
async fn verification_view_tx(tx: &mut Tx<'_>, task: &Task) -> Result<VerificationView> {
    let gate_result = task.gate_result_json.as_deref().and_then(|raw| {
        match serde_json::from_str::<TaskGateResult>(raw) {
            Ok(result) => Some(result),
            Err(error) => {
                tracing::warn!(task_id = %task.id, %error, "gate_result_json unreadable");
                None
            }
        }
    });
    let gate_op_present: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM operations WHERE kind = ?1 AND idempotency_key IN (?2, ?3))",
    )
    .bind(TASK_VERIFY_KIND)
    .bind(gate_attempt_key(&task.id, task.gate_attempt))
    .bind(gate_attempt_key(&task.id, task.gate_attempt + 1))
    .fetch_one(&mut **tx)
    .await?;
    Ok(verification_state(
        &task.id,
        task.gate_json.as_deref(),
        task.status,
        task.status_detail.as_deref(),
        task.gate_attempt,
        gate_result.as_ref(),
        gate_op_present,
        gate_runs_view_tx(tx, &task.id).await?,
    ))
}

/// D6: how many runs the worker's attempt admitted, and the highest-numbered finished one.
async fn gate_runs_view_tx(
    tx: &mut Tx<'_>,
    task_id: &str,
) -> Result<Option<super::verification::GateRunsView>> {
    let mut runs = gate_ops_of_attempt_tx(tx, task_id).await?;
    runs.retain(|op| op.kind == GateOpKind::Run);
    let mut last = None;
    for op in runs.iter().rev() {
        if let Some(outcome) = gate_op_outcome_tx(tx, op).await? {
            // The view shows no log path, so none is passed for a run that left no result.
            last = Some(terminal_result(
                op.number,
                std::path::Path::new(""),
                outcome,
            ));
            break;
        }
    }
    Ok(gate_runs_view(runs.len() as i64, last.as_ref()))
}
