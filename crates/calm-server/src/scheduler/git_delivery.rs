//! Kernel git delivery (#1727 S4 D2): the scheduler's half of the persistent hand-off.
//!
//! `task_git_deliveries` rows are written by the report transaction; this module re-submits a
//! row whose forge Operation is missing (same `operation_key`, so the runtime dedups the report
//! handler's own submission), waits for the Operation off the Track lock, and settles the row
//! once — a candidate row or the failure columns, plus one `task.git_delivery_settled` event —
//! guarded by `WHERE settlement IS NULL`.
use std::path::Path;

use calm_types::git_candidate::{DeliveryFailureCode, DeliverySettlement, DeliveryWakeReason};

use super::*;
use crate::db::sqlite::{append_decision_event_in_tx, task_get_tx};
use crate::db::write_in_tx_typed;
use crate::event::{BroadcastEnvelope, SYNC_EVENT_VERSION};
use crate::git_candidate::candidate::{CandidateRow, from_operation_result, resolve_ref_commit};
use crate::git_candidate::delivery::{
    DeliveryRow, UnsettledDelivery, classify_failure, delivery_by_id_tx, lease_for_delivery_tx,
    settle_candidate_tx, settle_failed_tx, submit_delivery, unresolved_failure,
    unsettled_deliveries_for_track_tx,
};
use crate::operation::forge_action_adapter::{
    FORGE_ACTION_KIND, ForgeActionPayload, ForgeActionResultFile, read_result_file,
};
use crate::operation::workspace_lease::WorkspaceLease;

/// What one settlement transaction writes: the candidate row, or the three failure facts.
enum Settlement {
    Candidate(Box<CandidateRow>),
    Failed {
        code: DeliveryFailureCode,
        reason: String,
        retry_allowed: bool,
    },
}

impl Scheduler {
    /// Fixtures-only reach-through to [`Scheduler::settle_git_delivery`], the second executor a
    /// test plays by hand.
    #[cfg(feature = "fixtures")]
    #[doc(hidden)]
    pub async fn settle_git_delivery_for_test(self: &Arc<Self>, delivery_id: &str) -> Result<()> {
        self.settle_git_delivery(delivery_id).await
    }

    /// `sweep_reconcile`'s authoritative discovery (F6.4): every Track holding an unsettled
    /// delivery row, whatever its tasks' status or its lifecycle — an ungated completion that
    /// crashed before submission leaves only a `done` row, which no non-terminal scan visits.
    pub(super) async fn unsettled_git_delivery_tracks(&self) -> Vec<String> {
        let Some(pool) = self.repo.sqlite_pool() else {
            return Vec::new();
        };
        match sqlx::query_scalar::<_, String>(
            "SELECT DISTINCT track_id FROM task_git_deliveries WHERE settlement IS NULL",
        )
        .fetch_all(&pool)
        .await
        {
            Ok(tracks) => tracks,
            Err(error) => {
                tracing::warn!(%error, "git delivery sweep failed; next tick retries");
                Vec::new()
            }
        }
    }

    /// Steps 1–2 of the D2 settlement table for every unsettled delivery of `track`: no
    /// Operation → submit under the row's key, then wait; Operation not terminal → wait; terminal
    /// → settle. Each delivery runs in its own spawned task under the `git-delivery:<id>`
    /// single-flight key, so a wait never occupies the Track scheduling lock.
    pub(super) async fn resume_git_deliveries(self: &Arc<Self>, track: &str) -> Result<()> {
        let track_id = track.to_string();
        let unsettled = write_in_tx_typed(self.repo.as_ref(), move |tx| {
            Box::pin(async move { unsettled_deliveries_for_track_tx(tx, &track_id).await })
        })
        .await?;
        for delivery in unsettled {
            let key = format!("git-delivery:{}", delivery.row.delivery_id);
            let Some(guard) = InflightGuard::acquire(&self.inflight, &key) else {
                continue;
            };
            let this = self.clone();
            tokio::spawn(async move {
                let _guard = guard;
                let delivery_id = delivery.row.delivery_id.clone();
                if let Err(error) = this.drive_git_delivery(delivery).await {
                    tracing::warn!(%error, %delivery_id, "git delivery remains unsettled; next pass retries");
                }
            });
        }
        Ok(())
    }

    async fn drive_git_delivery(self: &Arc<Self>, delivery: UnsettledDelivery) -> Result<()> {
        let Some(runtime) = self.operation_runtime.upgrade() else {
            return Ok(());
        };
        let op_id = match delivery.operation_id {
            Some(op_id) => op_id,
            None => {
                let lease = self.lease_for(&delivery.row).await?;
                submit_delivery(&runtime, &self.gate_logs_dir, &delivery.row, &lease)
                    .await?
                    .op_id
            }
        };
        runtime.wait(&op_id).await?;
        self.settle_git_delivery(&delivery.row.delivery_id).await
    }

    async fn lease_for(&self, delivery: &DeliveryRow) -> Result<WorkspaceLease> {
        let delivery = delivery.clone();
        write_in_tx_typed(self.repo.as_ref(), move |tx| {
            Box::pin(async move { lease_for_delivery_tx(tx, &delivery).await })
        })
        .await
    }

    /// Steps 3–5 of the D2 settlement table for one delivery whose forge Operation is terminal.
    ///
    /// Contract for every caller (this module's drive, and slice 4's `drive_gate` arm for a
    /// terminal-but-unsettled delivery): callable any number of times and concurrently — the
    /// settlement UPDATE is guarded by `WHERE settlement IS NULL`, a second executor's transaction
    /// rolls back with nothing written and no event appended, and a row that is already settled,
    /// has no Operation, or whose Operation is not terminal returns `Ok(())` without a transaction.
    /// The candidate is minted from the Operation result and the lease row, never from events,
    /// and the ref is confirmed in `lease.git_common_dir` before anything is written.
    pub(super) async fn settle_git_delivery(self: &Arc<Self>, delivery_id: &str) -> Result<()> {
        let Some(runtime) = self.operation_runtime.upgrade() else {
            return Ok(());
        };
        let id = delivery_id.to_string();
        let found = write_in_tx_typed(self.repo.as_ref(), move |tx| {
            Box::pin(async move {
                let Some(delivery) = delivery_by_id_tx(tx, &id).await? else {
                    return Ok(None);
                };
                if delivery.settlement.is_some() {
                    return Ok(None);
                }
                let lease = lease_for_delivery_tx(tx, &delivery).await?;
                Ok(Some((delivery, lease)))
            })
        })
        .await?;
        let Some((delivery, lease)) = found else {
            return Ok(());
        };
        let Some(op) = runtime
            .find_by_kind_and_idempotency(FORGE_ACTION_KIND, &delivery.forge_idempotency_key)
            .await?
        else {
            return Ok(());
        };
        let Some(result) = runtime.operation_result(&op.id).await? else {
            return Ok(());
        };
        let workspace_present = Path::new(&lease.path).is_dir();
        let settlement = match result.outcome {
            OperationOutcome::Succeeded { result }
            | OperationOutcome::SucceededViaCollision { result, .. } => {
                candidate_settlement(&delivery, &lease, &result).await?
            }
            OperationOutcome::Failed {
                last_error_class, ..
            } => {
                let file = result_file_of(&op.payload).await;
                let (code, reason, retry_allowed) = classify_failure(
                    file.as_ref(),
                    last_error_class.as_deref(),
                    workspace_present,
                );
                Settlement::Failed {
                    code,
                    reason,
                    retry_allowed,
                }
            }
            OperationOutcome::Stuck { .. } => {
                let file = result_file_of(&op.payload).await;
                let (code, reason, retry_allowed) =
                    classify_failure(file.as_ref(), None, workspace_present);
                Settlement::Failed {
                    code,
                    reason,
                    retry_allowed,
                }
            }
        };
        let settled = match self.settle_tx(delivery, settlement).await {
            Ok(envelope) => envelope,
            Err(error) if is_race_lost(&error) => return Ok(()),
            Err(error) => return Err(error),
        };
        // The live arm of `task.git_delivery_settled` pushes the planner and pokes this scheduler;
        // no second poke here (a second pass must be a no-op, and one is enough to prove it).
        self.events.emit_envelope(settled);
        Ok(())
    }

    /// One immediate transaction: the wake disposition from the tasks row, the settlement event
    /// through the authorized append seam, then the guarded settlement UPDATE (and the candidate
    /// row). A guard miss is the concurrent second executor: the transaction rolls back.
    async fn settle_tx(
        self: &Arc<Self>,
        delivery: DeliveryRow,
        settlement: Settlement,
    ) -> Result<BroadcastEnvelope> {
        write_in_tx_typed(self.repo.as_ref(), move |tx| {
            Box::pin(async move {
                let task = task_get_tx(tx, &delivery.producer_attempt_id)
                    .await?
                    .ok_or_else(|| {
                        CalmError::Internal(format!(
                            "delivery {} names attempt {} which has no tasks row",
                            delivery.delivery_id, delivery.producer_attempt_id
                        ))
                    })?;
                let track = crate::track_lifecycle::track_get_tx(
                    tx,
                    &TrackId::from(delivery.track_id.clone()),
                )
                .await?;
                let (result, wake_reason) = match &settlement {
                    Settlement::Candidate(candidate) => (
                        DeliverySettlement::Candidate {
                            candidate_id: candidate.candidate_id.clone(),
                            commit_sha: candidate.commit_sha.clone(),
                            base_sha: candidate.base_sha.clone(),
                            base_is_ancestor: candidate.base_is_ancestor,
                        },
                        if task.gate_json.is_none() {
                            DeliveryWakeReason::UngatedCandidate
                        } else if task.status != TaskStatus::Verifying {
                            DeliveryWakeReason::GateAlreadyTerminal
                        } else {
                            DeliveryWakeReason::DeferredToGate
                        },
                    ),
                    Settlement::Failed {
                        code,
                        reason,
                        retry_allowed,
                    } => (
                        DeliverySettlement::Failed {
                            code: *code,
                            reason: reason.clone(),
                            retry_allowed: *retry_allowed,
                        },
                        DeliveryWakeReason::Failed,
                    ),
                };
                let ordinal = u32::try_from(delivery.ordinal).map_err(|_| {
                    CalmError::Internal(format!(
                        "delivery {} ordinal {} is not a u32",
                        delivery.delivery_id, delivery.ordinal
                    ))
                })?;
                let actor = ActorId::Kernel;
                let scope = EventScope::Track {
                    track: track.id,
                    area: track.area_id,
                };
                let event = Event::TaskGitDeliverySettled {
                    task_id: task.id.clone(),
                    idempotency_key: task.id,
                    track_id: TrackId::from(delivery.track_id.clone()),
                    card_id: crate::ids::CardId::from(delivery.card_id.clone()),
                    delivery_id: delivery.delivery_id.clone(),
                    ordinal,
                    result,
                    wake_reason,
                };
                let event_id =
                    append_decision_event_in_tx(tx, &actor, &scope, None, &event).await?;
                let rows = match &settlement {
                    Settlement::Candidate(candidate) => {
                        settle_candidate_tx(
                            tx,
                            &delivery.delivery_id,
                            event_id,
                            wake_reason,
                            candidate,
                        )
                        .await?
                    }
                    Settlement::Failed {
                        code,
                        reason,
                        retry_allowed,
                    } => {
                        settle_failed_tx(
                            tx,
                            &delivery.delivery_id,
                            event_id,
                            *code,
                            reason,
                            *retry_allowed,
                        )
                        .await?
                    }
                };
                if rows == 0 {
                    return Err(race_lost_err());
                }
                Ok(BroadcastEnvelope {
                    id: event_id,
                    event_version: SYNC_EVENT_VERSION,
                    actor,
                    scope,
                    event,
                })
            })
        })
        .await
    }
}

/// Step 3: the candidate the result names, confirmed against the ref in the lease's common dir;
/// a result the ref does not confirm (or one the kernel cannot read) settles as `unresolved`.
async fn candidate_settlement(
    delivery: &DeliveryRow,
    lease: &WorkspaceLease,
    result: &Value,
) -> Result<Settlement> {
    let failed = |detail: String| {
        let (code, reason, retry_allowed) = unresolved_failure(&detail);
        Settlement::Failed {
            code,
            reason,
            retry_allowed,
        }
    };
    let candidate = match from_operation_result(
        delivery,
        lease,
        result.get("event").unwrap_or(&Value::Null),
        now_ms(),
    ) {
        Ok(candidate) => candidate,
        Err(error) => return Ok(failed(error.to_string())),
    };
    let Some(base) = lease.base.as_ref() else {
        return Ok(failed(format!(
            "lease {} has no recorded base",
            lease.lease_id
        )));
    };
    let resolved = resolve_ref_commit(&base.git_common_dir, &candidate.ref_name).await?;
    if resolved.as_deref() != Some(candidate.commit_sha.as_str()) {
        return Ok(failed(format!(
            "ref {} does not resolve to the reported commit {}",
            candidate.ref_name, candidate.commit_sha
        )));
    }
    Ok(Settlement::Candidate(Box::new(candidate)))
}

/// `<result_path>.code` / `.stdout` of the forge action, when the wrapper left them.
async fn result_file_of(payload: &Value) -> Option<ForgeActionResultFile> {
    let payload: ForgeActionPayload = serde_json::from_value(payload.clone()).ok()?;
    read_result_file(&payload.result_path).await.ok()
}
