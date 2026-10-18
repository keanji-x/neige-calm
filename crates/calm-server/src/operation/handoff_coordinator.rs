//! Durable internal user request. No route, tool or model can create an approval.
use serde::{Deserialize, Serialize};

use super::handoff_activation::ActivationResult;
use super::handoff_cutover::{CutoverResult, conflict};
use super::{OperationKey, OperationRuntime};
use crate::error::Result;
use crate::harness::handoff_pause::PauseRequest;
use crate::ids::{ActorId, CardId, TrackId};

pub(crate) const HANDOFF_COORDINATOR: &str = "handoff-workflow";

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
    pub version: u32,
    pub request: UserHandoffRequest,
    pub source: PauseRequest,
    pub target: PauseRequest,
    pub plan_id: String,
    pub new_session_id: String,
}

pub(crate) use super::handoff_state::*;

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
    let key = request.operation_key()?;
    let existing = runtime.keyed_replay(HANDOFF_COORDINATOR, &key).await?;
    if existing.is_none()
        && let Some(id) = runtime.keyed_replay("handoff-coordinator", &key).await?
    {
        return Ok(id);
    }
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
    if let Some(id) = existing {
        return Ok(id);
    }
    let id =
        match super::handoff_coordinator_sql::admit(&runtime.repo.sqlite_pool(), request.clone())
            .await?
        {
            Admission::Original(id) => id,
            Admission::Replay(id) => return Ok(id),
        };
    #[cfg(feature = "fixtures")]
    crate::test_seams::pause_point("handoff-begin-admitted", &request.key).await;
    // This capability lives only in the original begin future. It is neither serializable
    // nor installed in a runtime registry and disappears when the caller is dropped.
    ORIGINAL_BEGIN
        .scope(
            LiveAdmission {
                operation_id: id.clone(),
            },
            runtime.drive_target_locked(&id),
        )
        .await?;
    Ok(id)
}

/// Test scheduling seam; production admission is only `begin` with immediate driver ownership.
#[cfg(test)]
pub(crate) async fn admit_for_test(
    pool: &sqlx::SqlitePool,
    request: UserHandoffRequest,
) -> Result<String> {
    super::handoff_coordinator_sql::admit(pool, request)
        .await
        .map(|admission| match admission {
            Admission::Original(id) | Admission::Replay(id) => id,
        })
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
    let Some(id) = runtime.keyed_replay(HANDOFF_COORDINATOR, &key).await? else {
        return super::handoff_legacy::query_coordinator(runtime, request).await;
    };
    let op = runtime
        .repo
        .get_operation(&id)
        .await?
        .ok_or_else(conflict)?;
    let input: CoordinatorInput = serde_json::from_value(op.payload.clone())?;
    if input.version != 2 {
        return Err(conflict());
    }
    let Some(output) = op.tx_output else {
        return Ok(match op.last_error {
            Some(reason) => CoordinatorResult::NeedsRecovery { reason },
            None => CoordinatorResult::Preparing,
        });
    };
    let progress: Progress = serde_json::from_value(output.result)?;
    match progress {
        Progress::Committed {
            identity,
            resources,
        } => Ok(CoordinatorResult::Forward {
            cutover: Box::new(identity.result(&input, &op.id)),
            activation: resources.result(op.last_error.as_deref()),
        }),
        Progress::Activated { identity } => Ok(CoordinatorResult::Forward {
            cutover: Box::new(identity.result(&input, &op.id)),
            activation: ActivationResult::Activated {
                session_id: input.new_session_id,
            },
        }),
        Progress::Finished { .. } => Ok(CoordinatorResult::CancelledOrRefused),
        Progress::Compensating { reason, .. } => Ok(CoordinatorResult::NeedsRecovery { reason }),
        _ => Ok(match op.last_error {
            Some(reason) => CoordinatorResult::NeedsRecovery { reason },
            None => CoordinatorResult::Preparing,
        }),
    }
}

/// Cancellation is retained independently of whichever awaited effect is in flight.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "internal audited service; no external ingress")
)]
pub(crate) async fn cancel(runtime: &OperationRuntime, request: &UserHandoffRequest) -> Result<()> {
    let key = request.operation_key()?;
    let id = match runtime.keyed_replay(HANDOFF_COORDINATOR, &key).await? {
        Some(id) => id,
        None => runtime
            .keyed_replay("handoff-coordinator", &key)
            .await?
            .ok_or_else(conflict)?,
    };
    sqlx::query("UPDATE planner_handoff_coordinators SET cancel_requested=1 WHERE operation_id=?1")
        .bind(id)
        .execute(&runtime.repo.sqlite_pool())
        .await?;
    Ok(())
}

/// Explicit generation of the original owner; never reconstructs live admission.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "internal audited service; no external ingress")
)]
pub(crate) async fn retry(
    runtime: &OperationRuntime,
    request: &UserHandoffRequest,
    attempt: i32,
) -> Result<CoordinatorResult> {
    let observed = query(runtime, request).await?;
    if matches!(
        observed,
        CoordinatorResult::CancelledOrRefused
            | CoordinatorResult::Forward {
                activation: ActivationResult::Activated { .. },
                ..
            }
    ) {
        return Ok(observed);
    }
    let key = request.operation_key()?;
    if runtime
        .keyed_replay(HANDOFF_COORDINATOR, &key)
        .await?
        .is_none()
    {
        let _drive = runtime.drive_mutex.try_lock().map_err(|_| conflict())?;
        super::handoff_legacy::adopt_coordinator(runtime, request, attempt).await?;
    }
    runtime
        .retry_interaction(HANDOFF_COORDINATOR, &key, attempt)
        .await?;
    query(runtime, request).await
}

pub(super) enum Admission {
    Original(String),
    Replay(String),
}

struct LiveAdmission {
    operation_id: String,
}
tokio::task_local! { static ORIGINAL_BEGIN: LiveAdmission; }
pub(super) fn live_admission(op: &super::Operation) -> bool {
    ORIGINAL_BEGIN
        .try_with(|live| live.operation_id == op.id)
        .unwrap_or(false)
}
