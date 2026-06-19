use std::{io, path::Path};

use sqlx::{Row, SqlitePool};

use crate::db::sqlite::{append_decision_event_in_tx, begin_immediate_tx};
use crate::db::{RepoEventWrite, write_in_tx_typed};
use crate::error::{CalmError, Result};
use crate::event::{BroadcastEnvelope, Event, EventBus, EventScope, SYNC_EVENT_VERSION};
use crate::ids::{ActorId, CardId, CoveId, WaveId};
use crate::model::{new_id, now_ms};
use crate::proc_identity::read_boot_id;
use calm_truth::decision_gate::PermissiveGate;

use super::{TimestampMs, Tx};

#[derive(Clone, Debug)]
pub(crate) struct WorkspaceLease {
    pub lease_id: String,
    pub card_id: String,
    pub wave_id: String,
    pub path: String,
    pub state: String,
    pub boot_id: Option<String>,
}

pub(crate) async fn acquire_workspace_lease_tx(
    tx: &mut Tx<'_>,
    card_id: &str,
    wave_id: &str,
    lease_owner: &str,
) -> Result<(WorkspaceLease, BroadcastEnvelope)> {
    let lease_id = new_id();
    let path = workspace_lease_path_for(wave_id, card_id)?;
    // TODO(#760 slices 3/6): decide repo-root anchoring when git worktree
    // layering lands; slice 1 paths are relative to the server process cwd.
    let now = now_ms();
    let boot_id = read_boot_id();
    sqlx::query(
        r#"INSERT INTO workspace_leases (
               lease_id, card_id, wave_id, path, state, lease_owner,
               lease_until_ms, boot_id, created_at_ms, updated_at_ms
           )
           VALUES (?1, ?2, ?3, ?4, 'held', ?5, ?6, ?7, ?8, ?8)"#,
    )
    .bind(&lease_id)
    .bind(card_id)
    .bind(wave_id)
    .bind(&path)
    .bind(lease_owner)
    .bind(now + WORKSPACE_LEASE_MS)
    .bind(&boot_id)
    .bind(now)
    .execute(&mut **tx)
    .await?;

    std::fs::create_dir_all(&path).map_err(|e| {
        CalmError::Internal(format!("create workspace lease directory {path}: {e}"))
    })?;

    let scope = workspace_scope_tx(tx, card_id, wave_id).await?;
    let event = Event::WorkspaceLeased {
        wave_id: WaveId::from(wave_id.to_string()),
        card_id: CardId::from(card_id.to_string()),
        lease_id: lease_id.clone(),
        path: path.clone(),
    };
    let event_id = append_decision_event_in_tx(
        tx,
        &PermissiveGate,
        &ActorId::KernelDispatcher,
        &scope,
        None,
        &event,
    )
    .await?;

    let lease = WorkspaceLease {
        lease_id,
        card_id: card_id.to_string(),
        wave_id: wave_id.to_string(),
        path,
        state: "held".into(),
        boot_id,
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

pub(crate) async fn release_workspace_lease_by_id(
    pool: &SqlitePool,
    events: &EventBus,
    lease_id: &str,
) -> Result<bool> {
    let Some(lease) = workspace_lease_by_id(pool, lease_id).await? else {
        return Ok(false);
    };
    release_workspace_lease(pool, events, lease).await
}

pub(crate) async fn release_workspace_lease_for_card_repo(
    repo: &dyn RepoEventWrite,
    events: &EventBus,
    card_id: &str,
) -> Result<bool> {
    let Some(lease) = mark_workspace_lease_releasing_for_card_repo(repo, card_id).await? else {
        return Ok(false);
    };
    remove_workspace_dir_if_exists(&lease.path)?;
    let Some(envelope) = complete_workspace_lease_release_repo(repo, lease).await? else {
        return Ok(false);
    };
    events.emit_envelope(envelope);
    Ok(true)
}

pub(crate) async fn release_workspace_lease_for_card_tx(
    tx: &mut Tx<'_>,
    card_id: &str,
) -> Result<Vec<(ActorId, EventScope, Event)>> {
    let row = sqlx::query(
        r#"SELECT lease_id, card_id, wave_id, path, state, boot_id
           FROM workspace_leases
           WHERE card_id = ?1
             AND state IN ('held','releasing')
           ORDER BY created_at_ms DESC, lease_id DESC
           LIMIT 1"#,
    )
    .bind(card_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(row) = row else {
        return Ok(Vec::new());
    };
    let lease = row_to_workspace_lease(row)?;
    let mut events = Vec::new();
    if let Some(event) = release_workspace_lease_tx(tx, lease).await? {
        events.push(event);
    }
    Ok(events)
}

pub(crate) async fn reclaim_dead_workspace_leases_on_boot(
    pool: &SqlitePool,
    events: &EventBus,
) -> Result<usize> {
    let leases = active_workspace_leases(pool).await?;
    let current_boot_id = read_boot_id();
    let mut reclaimed = 0;
    for lease in leases {
        if lease.state == "held" {
            // Codex workers are daemon-resident threads, so operation
            // spawn_artifacts are not a liveness oracle. Boot reclaim only
            // takes leases from older machine boots; same-boot dead workers
            // are released by the reaper calling the lease helper directly.
            // The decision sink covers self-reported completion/failure, and
            // recoverable operations keep their cwd for recovery.
            if !workspace_lease_should_reclaim_on_boot(pool, &lease, current_boot_id.as_deref())
                .await?
            {
                continue;
            }
            let mut tx = begin_immediate_tx(pool).await?;
            let rows = sqlx::query(
                r#"UPDATE workspace_leases
                   SET state = 'releasing',
                       updated_at_ms = ?1
                   WHERE lease_id = ?2
                     AND state = 'held'"#,
            )
            .bind(now_ms())
            .bind(&lease.lease_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            tx.commit().await?;
            if rows == 0 {
                continue;
            }
        }
        if release_workspace_lease_by_id(pool, events, &lease.lease_id).await? {
            reclaimed += 1;
        }
    }
    Ok(reclaimed)
}

async fn release_workspace_lease(
    pool: &SqlitePool,
    events: &EventBus,
    lease: WorkspaceLease,
) -> Result<bool> {
    remove_workspace_dir_if_exists(&lease.path)?;

    let mut tx = begin_immediate_tx(pool).await?;
    let scope = workspace_scope_tx(&mut tx, &lease.card_id, &lease.wave_id).await?;
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
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if rows == 0 {
        tx.rollback().await?;
        return Ok(false);
    }

    let event = Event::WorkspaceReleased {
        wave_id: WaveId::from(lease.wave_id.clone()),
        card_id: CardId::from(lease.card_id.clone()),
        lease_id: lease.lease_id.clone(),
    };
    let event_id = append_decision_event_in_tx(
        &mut tx,
        &PermissiveGate,
        &ActorId::KernelDispatcher,
        &scope,
        None,
        &event,
    )
    .await?;
    tx.commit().await?;

    events.emit_envelope(BroadcastEnvelope {
        id: event_id,
        event_version: SYNC_EVENT_VERSION,
        actor: ActorId::KernelDispatcher,
        scope,
        event,
    });
    Ok(true)
}

async fn mark_workspace_lease_releasing_for_card_repo(
    repo: &dyn RepoEventWrite,
    card_id: &str,
) -> Result<Option<WorkspaceLease>> {
    let card_id = card_id.to_string();
    write_in_tx_typed(repo, move |tx| {
        let card_id = card_id.clone();
        Box::pin(async move {
            let row = sqlx::query(
                r#"SELECT lease_id, card_id, wave_id, path, state, boot_id
                   FROM workspace_leases
                   WHERE card_id = ?1
                     AND state IN ('held','releasing')
                   ORDER BY created_at_ms DESC, lease_id DESC
                   LIMIT 1"#,
            )
            .bind(&card_id)
            .fetch_optional(&mut **tx)
            .await?;
            let Some(row) = row else {
                return Ok(None);
            };
            let state: String = row.try_get("state")?;
            let lease = row_to_workspace_lease(row)?;
            if state == "held" {
                sqlx::query(
                    r#"UPDATE workspace_leases
                       SET state = 'releasing',
                           updated_at_ms = ?1
                       WHERE lease_id = ?2
                         AND state = 'held'"#,
                )
                .bind(now_ms())
                .bind(&lease.lease_id)
                .execute(&mut **tx)
                .await?;
            }
            Ok(Some(lease))
        })
    })
    .await
}

pub(crate) async fn release_workspace_leases_for_wave_tx(
    tx: &mut Tx<'_>,
    wave_id: &str,
) -> Result<Vec<(ActorId, EventScope, Event)>> {
    let rows = sqlx::query(
        r#"SELECT lease_id, card_id, wave_id, path, state, boot_id
           FROM workspace_leases
           WHERE wave_id = ?1
             AND state IN ('held','releasing')
           ORDER BY created_at_ms ASC, lease_id ASC"#,
    )
    .bind(wave_id)
    .fetch_all(&mut **tx)
    .await?;
    let leases: Vec<WorkspaceLease> = rows
        .into_iter()
        .map(row_to_workspace_lease)
        .collect::<Result<Vec<_>>>()?;
    let mut events = Vec::new();
    for lease in leases {
        if let Some(event) = release_workspace_lease_tx(tx, lease).await? {
            events.push(event);
        }
    }
    Ok(events)
}

async fn release_workspace_lease_tx(
    tx: &mut Tx<'_>,
    lease: WorkspaceLease,
) -> Result<Option<(ActorId, EventScope, Event)>> {
    remove_workspace_dir_if_exists(&lease.path)?;
    let scope = workspace_scope_tx(tx, &lease.card_id, &lease.wave_id).await?;
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
        return Ok(None);
    }
    Ok(Some((
        ActorId::KernelDispatcher,
        scope,
        Event::WorkspaceReleased {
            wave_id: WaveId::from(lease.wave_id),
            card_id: CardId::from(lease.card_id),
            lease_id: lease.lease_id,
        },
    )))
}

async fn complete_workspace_lease_release_repo(
    repo: &dyn RepoEventWrite,
    lease: WorkspaceLease,
) -> Result<Option<BroadcastEnvelope>> {
    write_in_tx_typed(repo, move |tx| {
        let lease = lease.clone();
        Box::pin(async move {
            let scope = workspace_scope_tx(tx, &lease.card_id, &lease.wave_id).await?;
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
                return Ok(None);
            }
            let event = Event::WorkspaceReleased {
                wave_id: WaveId::from(lease.wave_id.clone()),
                card_id: CardId::from(lease.card_id.clone()),
                lease_id: lease.lease_id.clone(),
            };
            let event_id = append_decision_event_in_tx(
                tx,
                &PermissiveGate,
                &ActorId::KernelDispatcher,
                &scope,
                None,
                &event,
            )
            .await?;
            Ok(Some(BroadcastEnvelope {
                id: event_id,
                event_version: SYNC_EVENT_VERSION,
                actor: ActorId::KernelDispatcher,
                scope,
                event,
            }))
        })
    })
    .await
}

async fn workspace_scope_tx(tx: &mut Tx<'_>, card_id: &str, wave_id: &str) -> Result<EventScope> {
    let cove_id: String = sqlx::query_scalar("SELECT cove_id FROM waves WHERE id = ?1")
        .bind(wave_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave {wave_id}")))?;
    Ok(EventScope::Card {
        card: CardId::from(card_id.to_string()),
        wave: WaveId::from(wave_id.to_string()),
        cove: CoveId::from(cove_id),
    })
}

async fn workspace_lease_by_id(
    pool: &SqlitePool,
    lease_id: &str,
) -> Result<Option<WorkspaceLease>> {
    let row = sqlx::query(
        r#"SELECT lease_id, card_id, wave_id, path, state, boot_id
           FROM workspace_leases
           WHERE lease_id = ?1
             AND state IN ('held','releasing')"#,
    )
    .bind(lease_id)
    .fetch_optional(pool)
    .await?;
    row.map(row_to_workspace_lease).transpose()
}

async fn active_workspace_leases(pool: &SqlitePool) -> Result<Vec<WorkspaceLease>> {
    let rows = sqlx::query(
        r#"SELECT lease_id, card_id, wave_id, path, state, boot_id
           FROM workspace_leases
           WHERE state IN ('held','releasing')
           ORDER BY created_at_ms ASC, lease_id ASC"#,
    )
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(row_to_workspace_lease).collect()
}

fn row_to_workspace_lease(row: sqlx::sqlite::SqliteRow) -> Result<WorkspaceLease> {
    Ok(WorkspaceLease {
        lease_id: row.try_get("lease_id")?,
        card_id: row.try_get("card_id")?,
        wave_id: row.try_get("wave_id")?,
        path: row.try_get("path")?,
        state: row.try_get("state")?,
        boot_id: row.try_get("boot_id")?,
    })
}

async fn workspace_lease_should_reclaim_on_boot(
    pool: &SqlitePool,
    lease: &WorkspaceLease,
    current_boot_id: Option<&str>,
) -> Result<bool> {
    let row = sqlx::query(
        r#"SELECT o.phase AS owner_phase
           FROM workspace_leases wl
           LEFT JOIN operations o ON o.id = wl.lease_owner
           WHERE wl.lease_id = ?1
             AND wl.state = 'held'"#,
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

fn operation_phase_is_recoverable(phase: &str) -> bool {
    matches!(
        phase,
        "pending"
            | "tx_committed"
            | "app_server_interact"
            | "spawn_started"
            | "spawn_succeeded"
            | "parked"
            | "compensating"
    )
}

fn remove_workspace_dir_if_exists(path: &str) -> Result<()> {
    let path = Path::new(path);
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(CalmError::Internal(format!(
            "remove workspace lease directory {}: {e}",
            path.display()
        ))),
    }
}

pub(crate) fn workspace_lease_path_for(wave_id: &str, card_id: &str) -> Result<String> {
    validate_path_segment("wave_id", wave_id)?;
    validate_path_segment("card_id", card_id)?;
    Ok(format!(".claude/worktrees/{wave_id}/{card_id}"))
}

fn validate_path_segment(label: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
    {
        return Err(CalmError::Internal(format!(
            "invalid workspace lease {label} path segment {value:?}"
        )));
    }
    Ok(())
}

const WORKSPACE_LEASE_MS: TimestampMs = 60_000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remove_workspace_dir_if_exists_treats_missing_as_success() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("already-gone");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::remove_dir_all(&path).unwrap();

        remove_workspace_dir_if_exists(path.to_str().unwrap()).unwrap();
    }
}
