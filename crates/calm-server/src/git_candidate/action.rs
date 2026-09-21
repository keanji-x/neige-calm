//! `calm.task.delivery{retry|abandon}` — the Planner's two actions on a failed Git delivery
//! (#1727 S4 slice 3, D2 "Planner action").
//!
//! One immediate transaction, in a fixed order: (1) replay first — the request key
//! `(track_id, producer_attempt_id, request_idempotency_key)` is looked up in the delivery rows
//! (a retry) and the abandonment rows (an abandon), scoped to the caller's Track so another
//! Track's Planner quoting this attempt and key never reads this Track's receipt; a hit with the
//! same `(action, expected_delivery_id, reason)` fingerprint returns the original receipt without
//! looking at the current state (a retry's `{delivery_id, ordinal, action}` from the retry row,
//! an abandon's five keys from the abandonment row), a different fingerprint is a `Conflict`;
//! (2) admission — the attempt is current in this Track, the expected delivery is the attempt's
//! latest, its effective state (`view::delivery_state`, fed the candidate row so a candidate is
//! never overlooked) is `failed`; (3) `retry` reads `retry_allowed` from the row (never the
//! result file, never an event), requires the lease directory, inserts the `ordinal + 1` row and
//! submits it after the commit; (4) `abandon` flips a gated `verifying` row through its own
//! guarded UPDATE (`task_abandon_delivery_tx`: 1 row → `failed` + one `task.failed` from
//! `KernelDispatcher` + the Track's `working → reviewing` auto-transition that every other
//! terminal flip makes, 0 rows → `already_terminal`), leaves an ungated `done` row alone
//! (`done_unchanged`), and writes the abandonment row with the observed status.
//!
//! Every refusal is a `Conflict` whose text starts with `refused:` and names the way out
//! (5.1.11); the handler maps it to `-32409`. Track lifecycle: the same rule as
//! `calm.task.verdict`, which admits a verdict on a Done Track (no lifecycle admission; G11), so
//! this action admits too.

use std::path::Path;
use std::sync::Arc;

use serde::Serialize;

use super::abandonment::{
    AbandonTaskOutcome, AbandonmentRow, abandonment_by_request_key_tx, abandonment_for_delivery_tx,
    insert_abandonment_tx, task_status_wire,
};
use super::candidate::candidate_for_attempt_tx;
use super::delivery::{
    DeliveryRow, delivery_by_id_tx, delivery_by_request_key_tx, delivery_latest_for_attempt_tx,
    insert_retry_delivery_tx, lease_for_delivery_tx, submit_delivery,
};
use super::view::{DeliveryFailure, DeliveryState, delivery_state};
use crate::db::sqlite::{
    TASK_STATUS_DETAIL_DELIVERY_ABANDONED, append_decision_events_in_tx, task_abandon_delivery_tx,
    task_attempt_current_tx, task_get_tx,
};
use crate::db::write_in_tx_typed;
use crate::error::{CalmError, Result};
use crate::event::{BroadcastEnvelope, Event, EventScope, SYNC_EVENT_VERSION};
use crate::ids::{ActorId, TrackId};
use crate::mcp_server::registry::{AppContext, ToolCallIdentity};
use crate::model::{Task, TaskStatus, TrackLifecycle, now_ms};
use crate::operation::Tx;
use crate::operation::workspace_lease::WorkspaceLease;
use crate::operation::workspace_lease::facts::{LeaseStates, latest_workspace_lease_for_card_tx};
use crate::track_lifecycle::auto_transition_if_current_in_tx;

/// The two actions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeliveryAction {
    Retry,
    Abandon,
}

impl DeliveryAction {
    pub(crate) fn wire_str(self) -> &'static str {
        match self {
            DeliveryAction::Retry => "retry",
            DeliveryAction::Abandon => "abandon",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "retry" => Some(DeliveryAction::Retry),
            "abandon" => Some(DeliveryAction::Abandon),
            _ => None,
        }
    }
}

/// The tool's arguments, parsed by the handler.
#[derive(Clone, Debug)]
pub(crate) struct DeliveryActionArgs {
    pub key: String,
    pub expected_attempt_id: String,
    pub expected_delivery_id: String,
    pub idempotency_key: String,
    pub action: DeliveryAction,
    pub reason: Option<String>,
}

/// The tool result. A retry is `{delivery_id, ordinal, action:"retry"}` — nothing happens to the
/// tasks row and nothing about it is persisted, so the receipt carries no task key (D2). An
/// abandon adds `task_outcome` and `task_status` exactly as the abandonment row stores them; the
/// first call and every replay read the same row, so the receipt is stable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct DeliveryActionReceipt {
    pub delivery_id: String,
    pub ordinal: i64,
    pub action: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_outcome: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_status: Option<&'static str>,
}

/// What the transaction hands the post-commit half: the retry row to submit, or the events the
/// abandon appended (`task.failed`, then the Track auto-transition's pair when it fired), in
/// append order.
enum AfterCommit {
    Nothing,
    Submit(Box<(DeliveryRow, WorkspaceLease)>),
    Broadcast(Vec<BroadcastEnvelope>),
}

fn refused(text: String) -> CalmError {
    CalmError::Conflict(text)
}

/// Apply one action for the Planner `identity` on its Track. See the module docs for the order.
pub(crate) async fn apply_delivery_action(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
    args: DeliveryActionArgs,
) -> Result<DeliveryActionReceipt> {
    let card_id = identity.card_id.clone();
    let card = ctx
        .repo
        .card_get(&card_id)
        .await?
        .ok_or_else(|| CalmError::Internal(format!("bound card {card_id} not found")))?;
    let track = ctx
        .repo
        .track_get(card.track_id.as_str())
        .await?
        .ok_or_else(|| {
            CalmError::Internal(format!(
                "track {} for card {card_id} not found",
                card.track_id.as_str()
            ))
        })?;
    let scope = EventScope::Track {
        track: track.id.clone(),
        area: track.area_id.clone(),
    };
    let track_id = track.id.to_string();
    let (receipt, after) = write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
        Box::pin(async move { apply_in_tx(tx, &track_id, &scope, args).await })
    })
    .await?;
    match after {
        AfterCommit::Nothing => {}
        AfterCommit::Submit(boxed) => {
            let (row, lease) = *boxed;
            // Same contract as `calm.task.complete`: the row is durable, the sweep re-submits
            // under the same key if this submission does not land.
            match ctx.operation_runtime.get().cloned() {
                Some(runtime) => {
                    if let Err(error) =
                        submit_delivery(&runtime, &ctx.gate_logs_dir, &row, &lease).await
                    {
                        tracing::warn!(
                            delivery_id = %row.delivery_id,
                            error = %error,
                            "retry delivery row persisted but its submission failed; the sweep resubmits"
                        );
                    }
                }
                None => tracing::warn!(
                    delivery_id = %row.delivery_id,
                    "retry delivery row persisted; operation runtime not bound, the sweep resubmits"
                ),
            }
            // No event carries a retry (`task.completed` carries the first delivery), so the
            // scheduler is poked directly: `resume_git_deliveries` drives the new row to its
            // settlement now instead of on the next reconcile sweep.
            match ctx.scheduler_poke.get() {
                Some(poke) => poke(track.id.clone()),
                None => tracing::debug!(
                    delivery_id = %row.delivery_id,
                    "scheduler poke not bound; the reconcile sweep drives the retry"
                ),
            }
        }
        AfterCommit::Broadcast(envelopes) => {
            for envelope in envelopes {
                ctx.events.emit_envelope(envelope);
            }
        }
    }
    Ok(receipt)
}

async fn apply_in_tx(
    tx: &mut Tx<'_>,
    track_id: &str,
    scope: &EventScope,
    args: DeliveryActionArgs,
) -> Result<(DeliveryActionReceipt, AfterCommit)> {
    let attempt_id = args.expected_attempt_id.as_str();
    // (1) Replay first: the request key decides before any state is read. Both lookups are
    // Track-scoped: a row of another Track is no hit, and admission below refuses the attempt as
    // not current in this Track.
    if let Some(row) =
        delivery_by_request_key_tx(tx, track_id, attempt_id, &args.idempotency_key).await?
    {
        let same = args.action == DeliveryAction::Retry
            && row.predecessor_delivery_id.as_deref() == Some(args.expected_delivery_id.as_str())
            && row.reason == args.reason;
        if !same {
            return Err(fingerprint_conflict(&args));
        }
        return Ok((retry_receipt(&row), AfterCommit::Nothing));
    }
    if let Some(row) =
        abandonment_by_request_key_tx(tx, track_id, attempt_id, &args.idempotency_key).await?
    {
        let same = args.action == DeliveryAction::Abandon
            && row.delivery_id == args.expected_delivery_id
            && row.reason == args.reason;
        if !same {
            return Err(fingerprint_conflict(&args));
        }
        let delivery = delivery_by_id_tx(tx, &row.delivery_id)
            .await?
            .ok_or_else(|| {
                CalmError::Internal(format!(
                    "abandonment of delivery {} has no delivery row",
                    row.delivery_id
                ))
            })?;
        return Ok((abandon_receipt(&row, &delivery), AfterCommit::Nothing));
    }

    // (2) Admission.
    let current = task_attempt_current_tx(tx, track_id, &args.key)
        .await?
        .ok_or_else(|| {
            refused(format!(
                "refused: task {} has no current attempt in this Track; declare a new task",
                args.key
            ))
        })?;
    if current.attempt_id != attempt_id {
        return Err(refused(format!(
            "refused: expected_attempt_id {attempt_id} is not the current attempt of task {} \
             (current: {}); read calm.plan.list and act on the current attempt",
            args.key, current.attempt_id
        )));
    }
    let task = task_row(tx, attempt_id).await?;
    let Some(latest) = delivery_latest_for_attempt_tx(tx, attempt_id).await? else {
        return Err(no_delivery_refusal(tx, &task).await?);
    };
    if latest.delivery_id != args.expected_delivery_id {
        return Err(refused(format!(
            "refused: expected_delivery_id {} is not the latest delivery of attempt {attempt_id} \
             (latest: {}); read calm.plan.list.candidate.delivery.delivery_id and act on it",
            args.expected_delivery_id, latest.delivery_id
        )));
    }
    let candidate = candidate_for_attempt_tx(tx, attempt_id).await?;
    let abandonment = abandonment_for_delivery_tx(tx, &latest.delivery_id)
        .await?
        .map(|row| row.facts());
    let state = delivery_state(
        task.status,
        task.status_detail.as_deref(),
        Some(&latest),
        candidate.as_ref(),
        abandonment.as_ref(),
    );
    let failure = admit_failed(&latest, state)?;

    match args.action {
        DeliveryAction::Retry => retry(tx, &latest, &failure, &args).await,
        DeliveryAction::Abandon => abandon(tx, track_id, scope, &task, &latest, &args).await,
    }
}

/// Why an attempt has no delivery row, in the words the Planner can act on: a legacy lease (no
/// kernel delivery exists for it), a worker that has not reported yet, an attempt that ended
/// (`failed`) without ever reporting. Anything else (a `verifying`/`done` row without its
/// delivery row, a `pending`/`canceled` attempt) keeps the generic sentence.
async fn no_delivery_refusal(tx: &mut Tx<'_>, task: &Task) -> Result<CalmError> {
    let attempt_id = task.id.as_str();
    let lease = match task.worker_card_id.as_deref() {
        Some(card_id) => latest_workspace_lease_for_card_tx(tx, card_id, LeaseStates::Any).await?,
        None => None,
    };
    if let Some(lease) = lease
        && lease.delivery_policy.is_none()
    {
        return Ok(refused(format!(
            "refused: attempt {attempt_id} has a legacy lease (no kernel delivery); nothing to \
             retry or abandon"
        )));
    }
    Ok(match task.status {
        TaskStatus::Dispatched | TaskStatus::Running => refused(format!(
            "refused: attempt {attempt_id} has not reported yet; wait for the worker's report"
        )),
        TaskStatus::Failed => refused(format!(
            "refused: attempt {attempt_id} ended without a delivery ({}); declare a new task",
            task.status_detail.as_deref().unwrap_or("no status detail")
        )),
        TaskStatus::Pending | TaskStatus::Verifying | TaskStatus::Done | TaskStatus::Canceled => {
            refused(format!(
                "refused: attempt {attempt_id} has no Git delivery; wait for the worker's report or \
             declare a new task"
            ))
        }
    })
}

/// The effective-state gate: only `failed` admits either action; every other state names its
/// own way out.
fn admit_failed(latest: &DeliveryRow, state: DeliveryState) -> Result<DeliveryFailure> {
    let id = &latest.delivery_id;
    match state {
        DeliveryState::Failed { failure, .. } => Ok(failure),
        DeliveryState::Pending { .. } => Err(refused(format!(
            "refused: delivery {id} is pending; wait for task.git_delivery_settled"
        ))),
        DeliveryState::Committed { candidate_id, .. }
        | DeliveryState::NoChange { candidate_id, .. } => Err(refused(format!(
            "refused: attempt already has candidate {candidate_id}; accept it with calm.task.verdict"
        ))),
        DeliveryState::Abandoned { .. } => Err(refused(format!(
            "refused: delivery {id} was abandoned; declare a new task"
        ))),
        DeliveryState::Inconsistent { mismatches } => Err(refused(format!(
            "refused: delivery {id} rows are inconsistent ({}); nothing here can be retried or \
             abandoned, declare a new task",
            mismatches.join(", ")
        ))),
        DeliveryState::NotReported | DeliveryState::EndedWithoutDelivery { .. } => {
            Err(refused(format!(
                "refused: delivery {id} has no settled failure to act on; wait for \
                 task.git_delivery_settled or declare a new task"
            )))
        }
    }
}

async fn retry(
    tx: &mut Tx<'_>,
    latest: &DeliveryRow,
    failure: &DeliveryFailure,
    args: &DeliveryActionArgs,
) -> Result<(DeliveryActionReceipt, AfterCommit)> {
    if !failure.retry_allowed {
        return Err(refused(format!(
            "refused: delivery {} is not retryable ({}); abandon it with action:\"abandon\" or \
             declare a new task",
            latest.delivery_id,
            failure.code.wire_str()
        )));
    }
    let lease = lease_for_delivery_tx(tx, latest).await?;
    if !Path::new(&lease.path).is_dir() {
        return Err(refused(format!(
            "refused: delivery {} cannot be retried, workspace {} is missing; abandon it with \
             action:\"abandon\" or declare a new task",
            latest.delivery_id, lease.path
        )));
    }
    let row = insert_retry_delivery_tx(
        tx,
        latest,
        &args.idempotency_key,
        args.reason.as_deref(),
        now_ms(),
    )
    .await?;
    let receipt = retry_receipt(&row);
    Ok((receipt, AfterCommit::Submit(Box::new((row, lease)))))
}

async fn abandon(
    tx: &mut Tx<'_>,
    track_id: &str,
    scope: &EventScope,
    task: &Task,
    latest: &DeliveryRow,
    args: &DeliveryActionArgs,
) -> Result<(DeliveryActionReceipt, AfterCommit)> {
    let now = now_ms();
    let mut after = AfterCommit::Nothing;
    let (task_outcome, task_status) = if task.gate_json.is_some() {
        let rows = task_abandon_delivery_tx(tx, &task.id, track_id, now).await?;
        if rows == 1 {
            let reason = match args.reason.as_deref() {
                Some(reason) if !reason.is_empty() => {
                    format!("{TASK_STATUS_DETAIL_DELIVERY_ABANDONED}: {reason}")
                }
                _ => TASK_STATUS_DETAIL_DELIVERY_ABANDONED.to_string(),
            };
            let actor = ActorId::KernelDispatcher;
            let mut events = vec![Event::TaskFailed {
                idempotency_key: task.id.clone(),
                reason,
                details: None,
                agent_message: None,
            }];
            // The terminal flip promotes the Track exactly as the gate verdict, the worker
            // failure and the reaper do (`working → reviewing` when it is `working`), in this
            // transaction; the pair rides behind the `task.failed`.
            if let Some(auto_events) = auto_transition_if_current_in_tx(
                tx,
                &TrackId::from(track_id.to_string()),
                TrackLifecycle::Working,
                TrackLifecycle::Reviewing,
                &actor,
                Some("[auto] delivery abandoned".to_string()),
            )
            .await?
            {
                events.extend(auto_events);
            }
            let ids = append_decision_events_in_tx(tx, &actor, scope, None, &events).await?;
            after = AfterCommit::Broadcast(
                ids.into_iter()
                    .zip(events)
                    .map(|(id, event)| BroadcastEnvelope {
                        id,
                        event_version: SYNC_EVENT_VERSION,
                        actor: actor.clone(),
                        scope: scope.clone(),
                        event,
                    })
                    .collect(),
            );
            (AbandonTaskOutcome::Failed, TaskStatus::Failed)
        } else {
            // The gate flipped the row first (slice 3 without slice 4's gate admission).
            let observed = task_row(tx, &task.id).await?.status;
            (AbandonTaskOutcome::AlreadyTerminal, observed)
        }
    } else if task.status == TaskStatus::Done {
        (AbandonTaskOutcome::DoneUnchanged, TaskStatus::Done)
    } else {
        (AbandonTaskOutcome::AlreadyTerminal, task.status)
    };
    let row = AbandonmentRow {
        delivery_id: latest.delivery_id.clone(),
        track_id: track_id.to_string(),
        producer_attempt_id: latest.producer_attempt_id.clone(),
        request_idempotency_key: args.idempotency_key.clone(),
        reason: args.reason.clone(),
        task_outcome,
        task_status,
        created_at_ms: now,
    };
    insert_abandonment_tx(tx, &row).await?;
    Ok((abandon_receipt(&row, latest), after))
}

/// The retry receipt, from the retry row alone (first call and replay alike).
fn retry_receipt(row: &DeliveryRow) -> DeliveryActionReceipt {
    DeliveryActionReceipt {
        delivery_id: row.delivery_id.clone(),
        ordinal: row.ordinal,
        action: DeliveryAction::Retry.wire_str(),
        task_outcome: None,
        task_status: None,
    }
}

/// The abandon receipt, from the abandonment row and the delivery it names (first call and
/// replay alike): `task_outcome` / `task_status` are the row's stored values, never re-observed.
fn abandon_receipt(row: &AbandonmentRow, delivery: &DeliveryRow) -> DeliveryActionReceipt {
    DeliveryActionReceipt {
        delivery_id: row.delivery_id.clone(),
        ordinal: delivery.ordinal,
        action: DeliveryAction::Abandon.wire_str(),
        task_outcome: Some(row.task_outcome.wire_str()),
        task_status: Some(task_status_wire(row.task_status)),
    }
}

fn fingerprint_conflict(args: &DeliveryActionArgs) -> CalmError {
    refused(format!(
        "refused: idempotency_key {} was already used for attempt {} with a different \
         action/expected_delivery_id/reason; reuse the original arguments to replay, or a new key \
         for a new decision",
        args.idempotency_key, args.expected_attempt_id
    ))
}

async fn task_row(tx: &mut Tx<'_>, attempt_id: &str) -> Result<Task> {
    task_get_tx(tx, attempt_id)
        .await?
        .ok_or_else(|| CalmError::Internal(format!("attempt {attempt_id} has no tasks row")))
}
