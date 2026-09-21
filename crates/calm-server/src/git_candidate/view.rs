//! Pure derivations of the Planner read surface (`calm.plan.list.candidate`, D8): the effective
//! delivery state of one attempt from its rows (D2 derivation table) and the binding of one
//! task's current attempt (D8 first-match table). Inputs are rows; no Operation is consulted.

use calm_types::git_candidate::DeliveryFailureCode;
use calm_types::task_execution::IsolatedCodexSelection;
use calm_types::task_recovery::TASK_CHILD_TRACK_ROUTE;
use serde::Serialize;

use super::abandonment::abandonment_for_delivery_tx;
use super::candidate::{CandidateRow, candidate_for_attempt_tx};
use super::delivery::{DeliveryRow, DeliverySettled, delivery_latest_for_attempt_tx};
use crate::error::{CalmError, Result};
use crate::model::{Task, TaskKind, TaskStatus};
use crate::operation::Tx;
use crate::operation::workspace_lease::facts::{
    LeaseStates, WorkerWorktreeFacts, latest_workspace_lease_for_card_tx,
};
use crate::operation::workspace_lease::{DeliveryPolicy, WorkspaceLease};

/// The `integrity.mismatches` entries the delivery derivation can name.
pub(crate) const MISMATCH_DELIVERY_ROW_MISSING: &str = "delivery_row_missing";
pub(crate) const MISMATCH_ABANDONMENT_WITH_CANDIDATE: &str = "abandonment_with_candidate";
/// A `candidate` settlement without its candidate row, or a candidate row on a `failed`
/// settlement: both are impossible by construction (one transaction writes both) and are
/// reported as inconsistent rather than read as anything else.
pub(crate) const MISMATCH_CANDIDATE_ROW_MISSING: &str = "candidate_row_missing";
pub(crate) const MISMATCH_CANDIDATE_WITH_FAILED_SETTLEMENT: &str =
    "candidate_with_failed_settlement";

/// The three failure facts the delivery row carries (`failure_code`, `failure_reason`,
/// `retry_allowed`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct DeliveryFailure {
    pub code: DeliveryFailureCode,
    pub reason: String,
    pub retry_allowed: bool,
}

/// What an abandonment row says (`task_git_delivery_abandonments`, slice 3): the Planner's
/// reason, what the abandonment did to the tasks row and the status it observed
/// (`abandonment::AbandonmentRow::facts`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AbandonmentFacts {
    pub reason: Option<String>,
    pub task_outcome: String,
    pub task_status: String,
}

/// `delivery.state` — the eight-value effective state of one attempt's delivery.
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
    /// The latest delivery failed and no candidate exists.
    Failed {
        delivery_id: String,
        ordinal: i64,
        failure: DeliveryFailure,
    },
    /// The Planner abandoned the failed delivery; there will be no candidate.
    Abandoned {
        delivery_id: String,
        ordinal: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        task_outcome: String,
        task_status: String,
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
    abandonment: Option<&AbandonmentFacts>,
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
    match (&delivery.settlement, candidate, abandonment) {
        (None, _, _) => DeliveryState::Pending {
            delivery_id,
            ordinal,
        },
        (Some(DeliverySettled::Candidate { .. }), Some(candidate), None) => {
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
        (Some(DeliverySettled::Candidate { .. }), Some(_), Some(_)) => {
            DeliveryState::Inconsistent {
                mismatches: vec![MISMATCH_ABANDONMENT_WITH_CANDIDATE],
            }
        }
        (Some(DeliverySettled::Candidate { .. }), None, _) => DeliveryState::Inconsistent {
            mismatches: vec![MISMATCH_CANDIDATE_ROW_MISSING],
        },
        (
            Some(DeliverySettled::Failed {
                code,
                reason,
                retry_allowed,
                ..
            }),
            None,
            None,
        ) => DeliveryState::Failed {
            delivery_id,
            ordinal,
            failure: DeliveryFailure {
                code: *code,
                reason: reason.clone(),
                retry_allowed: *retry_allowed,
            },
        },
        (Some(DeliverySettled::Failed { .. }), None, Some(abandonment)) => {
            DeliveryState::Abandoned {
                delivery_id,
                ordinal,
                reason: abandonment.reason.clone(),
                task_outcome: abandonment.task_outcome.clone(),
                task_status: abandonment.task_status.clone(),
            }
        }
        (Some(DeliverySettled::Failed { .. }), Some(_), _) => DeliveryState::Inconsistent {
            mismatches: vec![MISMATCH_CANDIDATE_WITH_FAILED_SETTLEMENT],
        },
    }
}

/// Why a task's current attempt has no candidate binding at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NoBindingReason {
    Isolated,
    Terminal,
    ChildTrack,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub lease_state: String,
    pub removed: bool,
}

impl CandidateWorkspace {
    fn from_facts(facts: &WorkerWorktreeFacts) -> Self {
        CandidateWorkspace {
            path: facts.path.clone(),
            branch: facts.branch.clone(),
            lease_state: facts.state.clone(),
            removed: facts.removed,
        }
    }
}

/// `candidate.binding` — D8's three shapes.
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
    },
}

/// A task that declares isolated execution (`context.neige_execution` present, whatever it says:
/// a present invalid selection is never legacy).
fn declares_isolated(task: &Task) -> bool {
    let context: serde_json::Value = serde_json::from_str(&task.context_json).unwrap_or_default();
    match IsolatedCodexSelection::from_context(&context) {
        Ok(selection) => selection.is_some(),
        Err(_) => true,
    }
}

/// D8 first-match table. `facts` and `delivery_state` accompany `lease`: a lease row implies a
/// facts row (same table) and a kernel lease implies the caller derived the delivery state; a
/// caller that breaks either contract gets an `Err`, never a guessed binding.
pub(crate) fn candidate_binding(
    task: &Task,
    lease: Option<&WorkspaceLease>,
    facts: Option<&WorkerWorktreeFacts>,
    delivery_state: Option<DeliveryState>,
) -> Result<CandidateBinding> {
    if declares_isolated(task) {
        return Ok(CandidateBinding::None {
            reason: NoBindingReason::Isolated,
        });
    }
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
    match (lease.delivery_policy, lease.base.as_ref(), delivery_state) {
        (None, _, _) => Ok(CandidateBinding::Unbound {
            reason: UnboundReason::LegacyLease,
            workspace,
        }),
        (Some(DeliveryPolicy::Kernel), Some(base), Some(delivery)) => Ok(CandidateBinding::Bound {
            producer_attempt_id: task.id.clone(),
            base_sha: base.base_sha.clone(),
            workspace,
            delivery,
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

/// `calm.plan.list.candidate` for one current attempt: the lease row of its worker card (the
/// same latest row `facts` was derived from), the attempt's latest delivery row and candidate
/// row for a kernel lease, then the two pure derivations. `facts` is the entry's `worktree`
/// facts, read by the caller from the same card.
pub(crate) async fn candidate_view_tx(
    tx: &mut Tx<'_>,
    task: &Task,
    facts: Option<&WorkerWorktreeFacts>,
) -> Result<CandidateBinding> {
    let lease = match task.worker_card_id.as_deref() {
        Some(card_id) => latest_workspace_lease_for_card_tx(tx, card_id, LeaseStates::Any).await?,
        None => None,
    };
    let delivery = match lease.as_ref() {
        Some(lease) if lease.delivery_policy == Some(DeliveryPolicy::Kernel) => {
            let delivery = delivery_latest_for_attempt_tx(tx, &task.id).await?;
            let candidate = candidate_for_attempt_tx(tx, &task.id).await?;
            let abandonment = match delivery.as_ref() {
                Some(delivery) => abandonment_for_delivery_tx(tx, &delivery.delivery_id)
                    .await?
                    .map(|row| row.facts()),
                None => None,
            };
            Some(delivery_state(
                task.status,
                task.status_detail.as_deref(),
                delivery.as_ref(),
                candidate.as_ref(),
                abandonment.as_ref(),
            ))
        }
        _ => None,
    };
    candidate_binding(task, lease.as_ref(), facts, delivery)
}
