//! Durable internal user request. No route, tool or model can create an approval.
use serde::{Deserialize, Serialize};

use super::handoff_activation::ActivationResult;
use super::handoff_cutover::{CutoverResult, ValidatedCutoverPlan, conflict};
use super::{OperationKey, OperationRuntime};
use crate::error::Result;
use crate::harness::handoff_pause::{PauseReceipt, PauseRequest, ReleaseOutcome};
use crate::ids::{ActorId, CardId, TrackId};

pub(crate) const HANDOFF_COORDINATOR: &str = "handoff-coordinator";

/// Internal original user authorization, not a JSON readiness or approval assertion.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UserHandoffRequest {
    pub track_id: TrackId,
    pub source_card_id: CardId,
    pub source_session_id: String,
    pub target_card_id: CardId,
    pub target_session_id: String,
    pub source_generation: i64,
    pub target_generation: i64,
    pub track_generation: i64,
    pub user_audit_id: String,
    pub actor: ActorId,
    pub key: String,
}

impl UserHandoffRequest {
    pub(super) fn validate(&self) -> Result<()> {
        if self.source_generation < 0
            || self.target_generation < 0
            || self.track_generation < 0
            || self.actor != ActorId::User
            || [
                &self.key,
                &self.user_audit_id,
                &self.source_session_id,
                &self.target_session_id,
            ]
            .iter()
            .any(|s| s.is_empty())
            || self.source_card_id == self.target_card_id
            || self.source_session_id == self.target_session_id
        {
            return Err(conflict());
        }
        Ok(())
    }

    pub(super) fn operation_key(&self) -> Result<OperationKey> {
        self.validate()?;
        Ok(OperationKey {
            operation_key: self.key.clone(),
            idempotency_key: Some(self.key.clone()),
            payload_hash: crate::routes::idempotency_key::stable_payload_hash(
                &serde_json::to_value(self)?,
            )?,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CoordinatorInput {
    pub request: UserHandoffRequest,
    pub source: PauseRequest,
    pub target: PauseRequest,
    pub plan_id: String,
    pub new_session_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Stage {
    Reserved,
    SourceIntent,
    SourcePaused,
    TargetIntent,
    BothPaused,
    PlanRecorded,
    CommitIntent,
    Committed,
    Compensating,
    Finished,
    ReleasedNeedsRecovery,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Progress {
    pub stage: Stage,
    pub source_attempted: bool,
    pub target_attempted: bool,
    pub source_receipt: Option<PauseReceipt>,
    pub target_receipt: Option<PauseReceipt>,
    pub source_release: Option<ReleaseOutcome>,
    pub target_release: Option<ReleaseOutcome>,
    pub plan: Option<ValidatedCutoverPlan>,
    pub committed: Option<CutoverResult>,
    pub reason: Option<String>,
}

impl Progress {
    pub(super) fn reserved() -> Self {
        Self {
            stage: Stage::Reserved,
            source_attempted: false,
            target_attempted: false,
            source_receipt: None,
            target_receipt: None,
            source_release: None,
            target_release: None,
            plan: None,
            committed: None,
            reason: None,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum CoordinatorResult {
    Preparing,
    CancelledOrRefused,
    NeedsRecovery {
        reason: String,
    },
    Forward {
        cutover: Box<CutoverResult>,
        activation: ActivationResult,
    },
}

/// Admission is committed before driving. A dropped caller cannot erase accepted work.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "internal audited service; no external ingress")
)]
pub(crate) async fn begin(
    runtime: &OperationRuntime,
    request: UserHandoffRequest,
) -> Result<String> {
    let existing = runtime
        .keyed_replay(HANDOFF_COORDINATOR, &request.operation_key()?)
        .await?;
    let _drive = match runtime.drive_mutex.try_lock() {
        Ok(guard) => guard,
        Err(_) => {
            return existing.ok_or_else(|| {
                crate::error::CalmError::Conflict(
                    "Handoff operation driver is busy; submit a new explicit request when idle"
                        .into(),
                )
            });
        }
    };
    let id =
        super::handoff_coordinator_sql::admit(&runtime.repo.sqlite_pool(), request.clone()).await?;
    #[cfg(feature = "fixtures")]
    crate::test_seams::pause_point("handoff-begin-admitted", &request.key).await;
    runtime.drive_target_locked(&id).await?;
    if let CoordinatorResult::Forward { cutover, .. } = query(runtime, &request).await? {
        let CutoverResult::CutoverCommittedPendingActivation { operation_id, .. } = *cutover;
        if let Some(activation) = runtime
            .find_by_kind_and_idempotency(
                super::handoff_activation::HANDOFF_ACTIVATION,
                &operation_id,
            )
            .await?
        {
            runtime.drive_target_locked(&activation.id).await?;
        }
    }
    Ok(id)
}

/// Test scheduling seam; production admission is only `begin` with immediate driver ownership.
#[cfg(test)]
pub(crate) async fn admit_for_test(
    pool: &sqlx::SqlitePool,
    request: UserHandoffRequest,
) -> Result<String> {
    super::handoff_coordinator_sql::admit(pool, request).await
}

/// Read only: never clears holds, rearms a generation or retries a native submission.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "internal audited service; no external ingress")
)]
pub(crate) async fn query(
    runtime: &OperationRuntime,
    request: &UserHandoffRequest,
) -> Result<CoordinatorResult> {
    let key = request.operation_key()?;
    let id = runtime
        .keyed_replay(HANDOFF_COORDINATOR, &key)
        .await?
        .ok_or_else(conflict)?;
    let op = runtime
        .repo
        .get_operation(&id)
        .await?
        .ok_or_else(conflict)?;
    let Some(output) = op.tx_output else {
        return Ok(match op.last_error {
            Some(reason) => CoordinatorResult::NeedsRecovery { reason },
            None => CoordinatorResult::Preparing,
        });
    };
    let mut progress: Progress = serde_json::from_value(output.result)?;
    if progress.stage == Stage::CommitIntent {
        progress.committed = super::handoff_cutover::query_recorded(
            &runtime.repo.sqlite_pool(),
            progress.plan.as_ref().ok_or_else(conflict)?,
        )
        .await?;
    }
    if let Some(cutover) = progress.committed {
        let CutoverResult::CutoverCommittedPendingActivation {
            ref operation_id, ..
        } = cutover;
        let activation = match super::handoff_activation::query(runtime, operation_id).await {
            Ok(result) => result,
            Err(crate::error::CalmError::NotFound(reason)) => {
                ActivationResult::NeedsRecovery { reason }
            }
            Err(error) => return Err(error),
        };
        return Ok(CoordinatorResult::Forward {
            cutover: Box::new(cutover),
            activation,
        });
    }
    if progress.stage == Stage::Finished {
        return Ok(CoordinatorResult::CancelledOrRefused);
    }
    if let Some(reason) = progress.reason.or(op.last_error) {
        return Ok(CoordinatorResult::NeedsRecovery { reason });
    }
    Ok(CoordinatorResult::Preparing)
}

/// Cancellation is retained independently of whichever awaited effect is in flight.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "internal audited service; no external ingress")
)]
pub(crate) async fn cancel(runtime: &OperationRuntime, request: &UserHandoffRequest) -> Result<()> {
    let key = request.operation_key()?;
    let id = runtime
        .keyed_replay(HANDOFF_COORDINATOR, &key)
        .await?
        .ok_or_else(conflict)?;
    sqlx::query("UPDATE planner_handoff_coordinators SET cancel_requested=1 WHERE operation_id=?1")
        .bind(id)
        .execute(&runtime.repo.sqlite_pool())
        .await?;
    Ok(())
}

/// Explicit generation only. Postcommit activation holds use the activation owner's retry.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "internal audited service; no external ingress")
)]
pub(crate) async fn retry(
    runtime: &OperationRuntime,
    request: &UserHandoffRequest,
    attempt: i32,
) -> Result<super::OperationReceipt> {
    runtime
        .retry_interaction(HANDOFF_COORDINATOR, &request.operation_key()?, attempt)
        .await
}

/// Hold release is always a separate explicit generation of the existing activation owner.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "internal explicit activation retry")
)]
pub(crate) async fn retry_activation(
    runtime: &OperationRuntime,
    request: &UserHandoffRequest,
    attempt: i32,
) -> Result<super::OperationReceipt> {
    let CoordinatorResult::Forward { cutover, .. } = query(runtime, request).await? else {
        return Err(conflict());
    };
    let CutoverResult::CutoverCommittedPendingActivation { operation_id, .. } = *cutover;
    super::handoff_activation::retry(runtime, operation_id, attempt).await
}
