//! #1830 S2 D7 — the one release of a worker lease. It runs inside the transaction that ends the
//! attempt and freezes how the attempt ended there: it flips a held lease to `released` and, when
//! the attempt has no delivery row yet, inserts the first one with its `outcome`, so every
//! attempt (completed, failed or stopped) is committed in the track's checkout and the next
//! attempt starts from that commit.
//!
//! The attempt is the lease owner op's `idempotency_key`, else the task whose `worker_card_id`
//! is the lease card (a fixture lease has no owner op; compensation has no card stamp, so it
//! relies on the first).

use sqlx::{Row, SqlitePool};

use super::{
    DeliveryPolicy, WORKSPACE_LEASE_COLUMNS, WorkspaceLease, active_workspace_leases,
    append_workspace_events_tx, operation_phase_is_recoverable, release_workspace_lease_tx,
    row_to_workspace_lease,
};
use crate::db::sqlite::{
    TaskReporter, begin_immediate_tx, status_detail_with_reason, task_fail_from_worker_tx,
    task_get_tx,
};
use crate::db::{RepoEventWrite, write_in_tx_typed};
use crate::error::{CalmError, Result};
use crate::event::{Event, EventBus, EventScope};
use crate::git_candidate::delivery::{
    AttemptOutcome, delivery_latest_for_attempt_tx, insert_initial_delivery_tx,
};
use crate::ids::{ActorId, TrackId};
use crate::model::{TaskStatus, now_ms};
use crate::operation::Tx;
use crate::proc_identity::read_boot_id;

/// How the release that ends an attempt commits it. (A track or area delete and a superseded
/// stuck owner only flip the row: `super::release_workspace_lease_tx`.)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReleaseDelivery {
    /// The attempt ended this way: commit it.
    Commit(AttemptOutcome),
    /// Commit it with the outcome its terminal `tasks.status` says (the reaper's race-lost arm,
    /// the cleanup after a kill, a timeout flip that marked no live session).
    CommitAsTaskEnded,
}

/// The `reason` of the attempt a boot reclaim fails: the reaper's dead-worker detail class.
pub(crate) const BOOT_RECLAIM_REASON: &str =
    "the machine rebooted while the worker ran; its lease is released";

/// The card's latest active lease, released as `delivery` says. Nothing when the card holds none.
pub(crate) async fn release_workspace_lease_for_card_tx(
    tx: &mut Tx<'_>,
    card_id: &str,
    delivery: ReleaseDelivery,
) -> Result<Vec<(ActorId, EventScope, Event)>> {
    let sql = format!(
        "SELECT {WORKSPACE_LEASE_COLUMNS} FROM workspace_leases \
         WHERE card_id = ?1 AND state IN ('held','releasing') \
         ORDER BY created_at_ms DESC, lease_id DESC LIMIT 1"
    );
    let row = sqlx::query(&sql)
        .bind(card_id)
        .fetch_optional(&mut **tx)
        .await?;
    let Some(row) = row else {
        return Ok(Vec::new());
    };
    let lease = row_to_workspace_lease(row)?;
    release_lease_tx(tx, &lease, delivery).await
}

/// [`release_workspace_lease_for_card_tx`] in a transaction of its own; `true` when a lease was
/// released.
pub(crate) async fn release_workspace_lease_for_card_repo(
    repo: &dyn RepoEventWrite,
    events: &EventBus,
    card_id: &str,
    delivery: ReleaseDelivery,
) -> Result<bool> {
    let card_id = card_id.to_string();
    let envelopes = write_in_tx_typed(repo, move |tx| {
        let card_id = card_id.clone();
        Box::pin(async move {
            let events = release_workspace_lease_for_card_tx(tx, &card_id, delivery).await?;
            append_workspace_events_tx(tx, events).await
        })
    })
    .await?;
    let released = !envelopes.is_empty();
    for envelope in envelopes {
        events.emit_envelope(envelope);
    }
    Ok(released)
}

/// The compensation step (both worker adapters): the lease its prepare took, in a transaction of
/// its own; `true` when it was released.
pub(crate) async fn release_workspace_lease_by_id(
    pool: &SqlitePool,
    events: &EventBus,
    lease_id: &str,
    delivery: ReleaseDelivery,
) -> Result<bool> {
    let mut tx = begin_immediate_tx(pool).await?;
    let sql = format!(
        "SELECT {WORKSPACE_LEASE_COLUMNS} FROM workspace_leases \
         WHERE lease_id = ?1 AND state IN ('held','releasing')"
    );
    let Some(row) = sqlx::query(&sql)
        .bind(lease_id)
        .fetch_optional(&mut *tx)
        .await?
    else {
        tx.rollback().await?;
        return Ok(false);
    };
    let lease = row_to_workspace_lease(row)?;
    let released = release_lease_tx(&mut tx, &lease, delivery).await?;
    let envelopes = append_workspace_events_tx(&mut tx, released).await?;
    tx.commit().await?;
    let released = !envelopes.is_empty();
    for envelope in envelopes {
        events.emit_envelope(envelope);
    }
    Ok(released)
}

async fn release_lease_tx(
    tx: &mut Tx<'_>,
    lease: &WorkspaceLease,
    delivery: ReleaseDelivery,
) -> Result<Vec<(ActorId, EventScope, Event)>> {
    let events = release_workspace_lease_tx(tx, lease).await?;
    if events.is_empty() || lease.delivery_policy != Some(DeliveryPolicy::Kernel) {
        return Ok(events);
    }
    let Some(attempt_id) = lease_attempt_tx(tx, lease).await? else {
        return Ok(events);
    };
    if delivery_latest_for_attempt_tx(tx, &attempt_id)
        .await?
        .is_some()
    {
        return Ok(events);
    }
    let outcome = match delivery {
        ReleaseDelivery::Commit(outcome) => outcome,
        ReleaseDelivery::CommitAsTaskEnded => ended_outcome_tx(tx, &attempt_id).await?,
    };
    insert_initial_delivery_tx(tx, &attempt_id, lease, outcome, now_ms()).await?;
    Ok(events)
}

/// The attempt a lease belongs to (module docs).
async fn lease_attempt_tx(tx: &mut Tx<'_>, lease: &WorkspaceLease) -> Result<Option<String>> {
    let by_owner: Option<String> = sqlx::query_scalar(
        "SELECT t.id FROM workspace_leases wl \
         JOIN operations o ON o.id = wl.lease_owner \
         JOIN tasks t ON t.id = o.idempotency_key \
         WHERE wl.lease_id = ?1",
    )
    .bind(&lease.lease_id)
    .fetch_optional(&mut **tx)
    .await?;
    if by_owner.is_some() {
        return Ok(by_owner);
    }
    Ok(sqlx::query_scalar(
        "SELECT id FROM tasks WHERE worker_card_id = ?1 \
         ORDER BY created_at_ms DESC, id DESC LIMIT 1",
    )
    .bind(&lease.card_id)
    .fetch_optional(&mut **tx)
    .await?)
}

/// How an attempt whose `tasks` row is terminal ended.
async fn ended_outcome_tx(tx: &mut Tx<'_>, attempt_id: &str) -> Result<AttemptOutcome> {
    let task = task_get_tx(tx, attempt_id)
        .await?
        .ok_or_else(|| CalmError::Internal(format!("attempt {attempt_id} has no tasks row")))?;
    match task.status {
        TaskStatus::Done | TaskStatus::Verifying => Ok(AttemptOutcome::Completed),
        TaskStatus::Failed => Ok(AttemptOutcome::Failed),
        TaskStatus::Canceled => Ok(AttemptOutcome::Canceled),
        TaskStatus::Pending | TaskStatus::Dispatched | TaskStatus::Running => {
            Err(CalmError::Internal(format!(
                "attempt {attempt_id} is still {:?}; its lease is not released",
                task.status
            )))
        }
    }
}

/// Boot reclaim (kept from before #1830 S2, called by the operation driver at boot): a lease from
/// an older machine boot whose owner op is not recoverable. One transaction per lease fails the
/// owner attempt (CAS `dispatched|running`) with the reaper's dead-worker detail, which wakes the
/// Planner, and releases the lease with `outcome = 'interrupted'`. It is the one releaser for a
/// stuck owner and for a rebooted `exited` session.
pub(crate) async fn reclaim_dead_workspace_leases_on_boot(
    pool: &SqlitePool,
    events: &EventBus,
) -> Result<usize> {
    let current_boot_id = read_boot_id();
    let mut reclaimed = 0;
    for lease in active_workspace_leases(pool).await? {
        if !lease_should_reclaim_on_boot(pool, &lease, current_boot_id.as_deref()).await? {
            continue;
        }
        let mut tx = begin_immediate_tx(pool).await?;
        let mut released = fail_lease_attempt_tx(&mut tx, &lease).await?;
        let lease_events = release_lease_tx(
            &mut tx,
            &lease,
            ReleaseDelivery::Commit(AttemptOutcome::Interrupted),
        )
        .await?;
        if lease_events.is_empty() {
            tx.rollback().await?;
            continue;
        }
        released.extend(lease_events);
        let envelopes = append_workspace_events_tx(&mut tx, released).await?;
        tx.commit().await?;
        for envelope in envelopes {
            events.emit_envelope(envelope);
        }
        reclaimed += 1;
    }
    Ok(reclaimed)
}

/// Codex workers are daemon-resident threads, so operation spawn artifacts are not a liveness
/// oracle; boot reclaim only takes leases from older machine boots.
async fn lease_should_reclaim_on_boot(
    pool: &SqlitePool,
    lease: &WorkspaceLease,
    current_boot_id: Option<&str>,
) -> Result<bool> {
    let row = sqlx::query(
        r#"SELECT o.phase AS owner_phase
           FROM workspace_leases wl
           LEFT JOIN operations o ON o.id = wl.lease_owner
           WHERE wl.lease_id = ?1
             AND wl.state IN ('held','releasing')"#,
    )
    .bind(&lease.lease_id)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(false);
    };
    let owner_phase: Option<String> = row.try_get("owner_phase")?;
    if owner_phase
        .as_deref()
        .is_some_and(operation_phase_is_recoverable)
    {
        return Ok(false);
    }
    Ok(matches!(
        (lease.boot_id.as_deref(), current_boot_id),
        (Some(lease_boot), Some(current_boot)) if lease_boot != current_boot
    ))
}

/// Fail the lease's attempt as the reaper fails a dead worker (`spawn-failed: <reason>`, a
/// kernel `task.failed`, the Track's `working → reviewing`); nothing when it is not
/// `dispatched`/`running`.
async fn fail_lease_attempt_tx(
    tx: &mut Tx<'_>,
    lease: &WorkspaceLease,
) -> Result<Vec<(ActorId, EventScope, Event)>> {
    let Some(attempt_id) = lease_attempt_tx(tx, lease).await? else {
        return Ok(Vec::new());
    };
    let rows = task_fail_from_worker_tx(
        tx,
        &attempt_id,
        &lease.track_id,
        TaskReporter::Kernel,
        &status_detail_with_reason("spawn-failed", BOOT_RECLAIM_REASON),
        now_ms(),
    )
    .await?;
    if rows == 0 {
        return Ok(Vec::new());
    }
    let track_id = TrackId::from(lease.track_id.clone());
    let track = crate::db::sqlite::track_get_tx(tx, &track_id).await?;
    let scope = EventScope::Track {
        track: track.id,
        area: track.area_id,
    };
    let actor = ActorId::KernelDispatcher;
    Ok(vec![(
        actor,
        scope,
        Event::TaskFailed {
            idempotency_key: attempt_id,
            reason: BOOT_RECLAIM_REASON.to_string(),
            details: None,
            agent_message: None,
        },
    )])
}
