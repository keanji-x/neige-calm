use std::path::Path;

use sqlx::{QueryBuilder, Row, Sqlite, SqlitePool};

use crate::db::sqlite::append_decision_event_in_tx;
use crate::error::{CalmError, Result};
use crate::event::{BroadcastEnvelope, Event, EventScope, SYNC_EVENT_VERSION};
use crate::ids::{ActorId, AreaId, CardId, TrackId};
use crate::model::{TaskAccess, new_id, now_ms};
use crate::proc_identity::read_boot_id;
use crate::workspace_service::sweep::WorkspaceTrackSweep;

use super::forge_action_adapter::FORGE_ACTION_KIND;
use super::{PhaseTag, TimestampMs, Tx};

pub(crate) mod base;
pub(crate) mod facts;
pub(crate) mod release;
pub(crate) mod upstream;
pub(crate) mod upstream_fetch;
#[cfg(test)]
mod upstream_fetch_tests;
#[cfg(test)]
mod upstream_resolve_tests;
#[cfg(test)]
pub(crate) mod upstream_tests;
pub(crate) mod worker;

pub(crate) use base::{DeliveryPolicy, LeaseBase};
pub(crate) use release::{
    ReleaseDelivery, reclaim_dead_workspace_leases_on_boot, release_workspace_lease_for_card_repo,
    release_workspace_lease_for_card_tx,
};
#[cfg(any(test, feature = "fixtures"))]
pub(crate) use worker::prepare_worker_lease_as_tx;
pub(crate) use worker::{WorkerLeasePlan, worker_branch_tx};

/// The one SELECT list every reader of a lease row uses
/// (`row_to_workspace_lease` takes columns by name at run time, so a column
/// missing from any one SELECT is a `ColumnNotFound` no compiler sees). The
/// calm-truth read `db/sqlite/read.rs` `workspace_lease_for_card` builds its
/// own five-field struct and is deliberately not on this list.
pub(crate) const WORKSPACE_LEASE_COLUMNS: &str = "lease_id, card_id, track_id, path, state, boot_id, \
     base_sha, base_source, base_attempt_id, canonical_path, git_common_dir, delivery_policy";

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WorkspaceLease {
    pub lease_id: String,
    pub card_id: String,
    pub track_id: String,
    pub path: String,
    pub state: String,
    pub boot_id: Option<String>,
    /// `None` for a row written before migration 0111 or by the fixtures-only
    /// plain lease (the all-NULL tuple); every lease a worker op takes since
    /// slice 1 has one.
    pub base: Option<LeaseBase>,
    /// `Some(Kernel)` for every lease a worker op takes since slice 2 (written
    /// in the same INSERT as `base`); `None` is legacy — no candidate binding.
    pub delivery_policy: Option<DeliveryPolicy>,
}

const RECOVERABLE_OPERATION_PHASES: &[PhaseTag] = &[
    PhaseTag::Pending,
    PhaseTag::TxCommitted,
    PhaseTag::AppServerInteract,
    PhaseTag::SpawnStarted,
    PhaseTag::SpawnSucceeded,
    PhaseTag::Parked,
    PhaseTag::Compensating,
];

/// Defensive track/area teardown fence for in-flight forge actions.
/// Non-transactional read before the teardown transaction, so it has a TOCTOU window; the forge-op parked-recovery contract remains the real backstop.
pub(crate) async fn track_has_active_forge_action(
    pool: &SqlitePool,
    track_id: &str,
) -> Result<bool> {
    any_track_has_active_forge_action(pool, &[track_id]).await
}

pub(crate) async fn any_track_has_active_forge_action(
    pool: &SqlitePool,
    track_ids: &[&str],
) -> Result<bool> {
    if track_ids.is_empty() {
        return Ok(false);
    }

    let mut query = QueryBuilder::<Sqlite>::new(
        r#"SELECT EXISTS(
             SELECT 1 FROM operations
             WHERE kind = "#,
    );
    query.push_bind(FORGE_ACTION_KIND);
    query.push(" AND target_type = 'track' AND target_id IN (");
    {
        let mut separated = query.separated(", ");
        for track_id in track_ids {
            separated.push_bind(*track_id);
        }
        separated.push_unseparated(") AND phase IN (");
    }
    {
        let mut separated = query.separated(", ");
        for phase in RECOVERABLE_OPERATION_PHASES {
            separated.push_bind(phase.as_str());
        }
        separated.push_unseparated("))");
    }

    let exists = query.build_query_scalar().fetch_one(pool).await?;
    Ok(exists)
}

/// INSERT the lease row of one worker attempt at the directory [`worker::prepare_worker_lease_tx`] chose
/// (#1830 S2 D2): `delivery_policy = 'kernel'` and the five base columns in the one INSERT, then
/// the track's workspace freeze and `workspace.leased`. Nothing is created on disk. A read-only
/// attempt's row (#1917) records no base, so it has no `delivery_policy` and its release writes no
/// delivery row.
pub(crate) async fn acquire_workspace_lease_tx(
    tx: &mut Tx<'_>,
    card_id: &str,
    track_id: &str,
    lease_owner: &str,
    plan: &WorkerLeasePlan,
) -> Result<(WorkspaceLease, BroadcastEnvelope)> {
    let base = match plan.access() {
        TaskAccess::ReadWrite => Some(&plan.base),
        TaskAccess::ReadOnly => None,
    };
    acquire_workspace_lease_at_path_tx(
        tx,
        card_id,
        track_id,
        lease_owner,
        &plan.path,
        base,
        plan.access(),
    )
    .await
}

/// A lease without a base (the legacy all-NULL tuple, `delivery_policy` NULL) at `path`, which is
/// created. Fixtures only.
#[cfg(any(test, feature = "fixtures"))]
pub(crate) async fn acquire_plain_workspace_lease_tx(
    tx: &mut Tx<'_>,
    card_id: &str,
    track_id: &str,
    lease_owner: &str,
    path: &Path,
) -> Result<(WorkspaceLease, BroadcastEnvelope)> {
    std::fs::create_dir_all(path).map_err(|e| {
        CalmError::Internal(format!(
            "create workspace lease directory {}: {e}",
            path.display()
        ))
    })?;
    acquire_workspace_lease_at_path_tx(
        tx,
        card_id,
        track_id,
        lease_owner,
        path,
        None,
        TaskAccess::ReadWrite,
    )
    .await
}

async fn acquire_workspace_lease_at_path_tx(
    tx: &mut Tx<'_>,
    card_id: &str,
    track_id: &str,
    lease_owner: &str,
    path: &Path,
    base: Option<&LeaseBase>,
    access: TaskAccess,
) -> Result<(WorkspaceLease, BroadcastEnvelope)> {
    let lease_id = new_id();
    let path_string = path.to_string_lossy().to_string();
    let now = now_ms();
    let boot_id = read_boot_id();
    let query = sqlx::query(
        r#"INSERT INTO workspace_leases (
               lease_id, card_id, track_id, path, state, lease_owner,
               lease_until_ms, boot_id, created_at_ms, updated_at_ms,
               base_sha, base_source, base_attempt_id, canonical_path, git_common_dir,
               delivery_policy, access_mode
           )
           VALUES (?1, ?2, ?3, ?4, 'held', ?5, ?6, ?7, ?8, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)"#,
    )
    .bind(&lease_id)
    .bind(card_id)
    .bind(track_id)
    .bind(&path_string)
    .bind(lease_owner)
    .bind(now + WORKSPACE_LEASE_MS)
    .bind(&boot_id)
    .bind(now);
    let delivery_policy = base.map(|_| DeliveryPolicy::Kernel);
    LeaseBase::bind_columns(query, base)?
        .bind(delivery_policy.map(DeliveryPolicy::as_column))
        .bind(access.as_str())
        .execute(&mut **tx)
        .await?;

    // Freeze the workspace before the first lease row exists: the lease path would dangle after a
    // rename and nothing re-anchors it. The system area is excluded inside the freeze itself.
    crate::db::sqlite::track_workspace_freeze_tx(tx, track_id, now).await?;

    let scope = workspace_scope_tx(tx, card_id, track_id).await?;
    let event = Event::WorkspaceLeased {
        track_id: TrackId::from(track_id.to_string()),
        card_id: CardId::from(card_id.to_string()),
        lease_id: lease_id.clone(),
        path: path_string.clone(),
    };
    let event_id =
        append_decision_event_in_tx(tx, &ActorId::KernelDispatcher, &scope, None, &event).await?;

    let lease = WorkspaceLease {
        lease_id,
        card_id: card_id.to_string(),
        track_id: track_id.to_string(),
        path: path_string,
        state: "held".into(),
        boot_id,
        base: base.cloned(),
        delivery_policy,
    };
    Ok((
        lease,
        BroadcastEnvelope {
            id: event_id,
            event_version: SYNC_EVENT_VERSION,
            actor: ActorId::KernelDispatcher,
            scope,
            event,
        },
    ))
}

/// Track/area delete: every active lease of the track is released with no delivery row (D7), and
/// what the post-commit sweep removes is captured in the same transaction.
pub(crate) async fn release_workspace_leases_for_track_tx(
    tx: &mut Tx<'_>,
    track_id: &str,
) -> Result<WorkspaceTrackRelease> {
    let sweep =
        crate::workspace_service::sweep::workspace_track_sweep_for_track_tx(tx, track_id).await?;
    let sql = format!(
        "SELECT {WORKSPACE_LEASE_COLUMNS} FROM workspace_leases \
         WHERE track_id = ?1 AND state IN ('held','releasing') \
         ORDER BY created_at_ms ASC, lease_id ASC"
    );
    let rows = sqlx::query(&sql)
        .bind(track_id)
        .fetch_all(&mut **tx)
        .await?;
    let leases: Vec<WorkspaceLease> = rows
        .into_iter()
        .map(row_to_workspace_lease)
        .collect::<Result<Vec<_>>>()?;
    let mut events = Vec::new();
    for lease in leases {
        events.extend(release_workspace_lease_tx(tx, &lease).await?);
    }
    Ok(WorkspaceTrackRelease { events, sweep })
}

/// Flip one lease row to `released` and name the `workspace.released` event; nothing when the row
/// is no longer active.
pub(super) async fn release_workspace_lease_tx(
    tx: &mut Tx<'_>,
    lease: &WorkspaceLease,
) -> Result<Vec<(ActorId, EventScope, Event)>> {
    let scope = workspace_scope_tx(tx, &lease.card_id, &lease.track_id).await?;
    let now = now_ms();
    let rows = sqlx::query(
        r#"UPDATE workspace_leases
           SET state = 'released',
               updated_at_ms = ?1,
               released_at_ms = COALESCE(released_at_ms, ?1)
           WHERE lease_id = ?2
             AND state IN ('held','releasing')"#,
    )
    .bind(now)
    .bind(&lease.lease_id)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if rows == 0 {
        return Ok(Vec::new());
    }
    Ok(vec![(
        ActorId::KernelDispatcher,
        scope,
        Event::WorkspaceReleased {
            track_id: TrackId::from(lease.track_id.clone()),
            card_id: CardId::from(lease.card_id.clone()),
            lease_id: lease.lease_id.clone(),
        },
    )])
}

#[derive(Clone, Debug)]
pub(crate) struct WorkspaceTrackRelease {
    pub(crate) events: Vec<(ActorId, EventScope, Event)>,
    pub(crate) sweep: Option<WorkspaceTrackSweep>,
}

pub(super) async fn append_workspace_events_tx(
    tx: &mut Tx<'_>,
    events: Vec<(ActorId, EventScope, Event)>,
) -> Result<Vec<BroadcastEnvelope>> {
    let mut envelopes = Vec::with_capacity(events.len());
    for (actor, scope, event) in events {
        let event_id = append_decision_event_in_tx(tx, &actor, &scope, None, &event).await?;
        envelopes.push(BroadcastEnvelope {
            id: event_id,
            event_version: SYNC_EVENT_VERSION,
            actor,
            scope,
            event,
        });
    }
    Ok(envelopes)
}

pub(super) async fn workspace_scope_tx(
    tx: &mut Tx<'_>,
    card_id: &str,
    track_id: &str,
) -> Result<EventScope> {
    let area_id: String = sqlx::query_scalar("SELECT area_id FROM tracks WHERE id = ?1")
        .bind(track_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {track_id}")))?;
    Ok(EventScope::Card {
        card: CardId::from(card_id.to_string()),
        track: TrackId::from(track_id.to_string()),
        area: AreaId::from(area_id),
    })
}

pub(super) async fn active_workspace_leases(pool: &SqlitePool) -> Result<Vec<WorkspaceLease>> {
    let sql = format!(
        "SELECT {WORKSPACE_LEASE_COLUMNS} FROM workspace_leases \
         WHERE state IN ('held','releasing') ORDER BY created_at_ms ASC, lease_id ASC"
    );
    let rows = sqlx::query(&sql).fetch_all(pool).await?;
    rows.into_iter().map(row_to_workspace_lease).collect()
}

pub(super) fn row_to_workspace_lease(row: sqlx::sqlite::SqliteRow) -> Result<WorkspaceLease> {
    Ok(WorkspaceLease {
        lease_id: row.try_get("lease_id")?,
        card_id: row.try_get("card_id")?,
        track_id: row.try_get("track_id")?,
        path: row.try_get("path")?,
        state: row.try_get("state")?,
        boot_id: row.try_get("boot_id")?,
        base: LeaseBase::from_row(&row)?,
        delivery_policy: DeliveryPolicy::from_row(&row)?,
    })
}

pub(super) fn operation_phase_is_recoverable(phase: &str) -> bool {
    RECOVERABLE_OPERATION_PHASES
        .iter()
        .any(|tag| tag.as_str() == phase)
}

const WORKSPACE_LEASE_MS: TimestampMs = 60_000;

#[cfg(test)]
mod tests;
