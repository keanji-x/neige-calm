use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Output},
};

#[cfg(test)]
use std::collections::BTreeSet;
#[cfg(test)]
use std::sync::{Mutex, OnceLock};

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkspaceLeaseTarget {
    pub repo_root: PathBuf,
    pub path: PathBuf,
    pub branch: String,
}

impl WorkspaceLeaseTarget {
    pub(crate) fn path_string(&self) -> String {
        self.path.to_string_lossy().to_string()
    }

    pub(crate) fn repo_root_string(&self) -> String {
        self.repo_root.to_string_lossy().to_string()
    }
}

pub(crate) async fn prepare_workspace_lease_target_tx(
    tx: &mut Tx<'_>,
    wave_id: &str,
    card_id: &str,
) -> Result<WorkspaceLeaseTarget> {
    validate_path_segment("wave_id", wave_id)?;
    validate_path_segment("card_id", card_id)?;
    let cwd: String = sqlx::query_scalar("SELECT cwd FROM waves WHERE id = ?1")
        .bind(wave_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave {wave_id}")))?;
    let repo_root = git_repo_root_for_wave_cwd(wave_id, &cwd)?;
    Ok(WorkspaceLeaseTarget {
        path: workspace_lease_path_for(&repo_root, wave_id, card_id)?,
        branch: workspace_slice_branch_for(wave_id, card_id)?,
        repo_root,
    })
}

pub(crate) async fn acquire_workspace_lease_tx(
    tx: &mut Tx<'_>,
    card_id: &str,
    wave_id: &str,
    lease_owner: &str,
    target: &WorkspaceLeaseTarget,
) -> Result<(WorkspaceLease, BroadcastEnvelope)> {
    acquire_workspace_lease_at_path_tx(
        tx,
        card_id,
        wave_id,
        lease_owner,
        &target.path,
        WorkspaceLeaseDirectoryMode::ParentOnly,
    )
    .await
}

pub(crate) async fn acquire_plain_workspace_lease_tx(
    tx: &mut Tx<'_>,
    card_id: &str,
    wave_id: &str,
    lease_owner: &str,
    path: &Path,
) -> Result<(WorkspaceLease, BroadcastEnvelope)> {
    acquire_workspace_lease_at_path_tx(
        tx,
        card_id,
        wave_id,
        lease_owner,
        path,
        WorkspaceLeaseDirectoryMode::Leaf,
    )
    .await
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkspaceLeaseDirectoryMode {
    ParentOnly,
    Leaf,
}

async fn acquire_workspace_lease_at_path_tx(
    tx: &mut Tx<'_>,
    card_id: &str,
    wave_id: &str,
    lease_owner: &str,
    path: &Path,
    directory_mode: WorkspaceLeaseDirectoryMode,
) -> Result<(WorkspaceLease, BroadcastEnvelope)> {
    let lease_id = new_id();
    let path_string = path.to_string_lossy().to_string();
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
    .bind(&path_string)
    .bind(lease_owner)
    .bind(now + WORKSPACE_LEASE_MS)
    .bind(&boot_id)
    .bind(now)
    .execute(&mut **tx)
    .await?;

    create_workspace_lease_directory(path, directory_mode)?;

    let scope = workspace_scope_tx(tx, card_id, wave_id).await?;
    let event = Event::WorkspaceLeased {
        wave_id: WaveId::from(wave_id.to_string()),
        card_id: CardId::from(card_id.to_string()),
        lease_id: lease_id.clone(),
        path: path_string.clone(),
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
        path: path_string,
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

fn create_workspace_lease_directory(path: &Path, mode: WorkspaceLeaseDirectoryMode) -> Result<()> {
    match mode {
        WorkspaceLeaseDirectoryMode::ParentOnly => {
            let parent = path.parent().ok_or_else(|| {
                CalmError::Internal(format!(
                    "workspace lease path {} has no parent",
                    path.display()
                ))
            })?;
            std::fs::create_dir_all(parent).map_err(|e| {
                CalmError::Internal(format!(
                    "create workspace lease parent directory {}: {e}",
                    parent.display()
                ))
            })
        }
        WorkspaceLeaseDirectoryMode::Leaf => std::fs::create_dir_all(path).map_err(|e| {
            CalmError::Internal(format!(
                "create workspace lease directory {}: {e}",
                path.display()
            ))
        }),
    }
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
    if let Err(error) = remove_workspace_worktree_for_lease(&lease) {
        tracing::warn!(
            lease_id = %lease.lease_id,
            card_id = %lease.card_id,
            path = %lease.path,
            error = %error,
            "online workspace lease release could not remove workspace worktree; marking lease released"
        );
    }
    let envelopes = complete_workspace_lease_release_repo(repo, lease).await?;
    if envelopes.is_empty() {
        return Ok(false);
    }
    for envelope in envelopes {
        events.emit_envelope(envelope);
    }
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
    events.extend(release_workspace_lease_tx(tx, lease).await?);
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
        if release_workspace_lease_on_boot(pool, events, &lease.lease_id).await? {
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
    remove_workspace_worktree_for_lease(&lease)?;
    complete_workspace_lease_release(pool, events, lease).await
}

async fn release_workspace_lease_on_boot(
    pool: &SqlitePool,
    events: &EventBus,
    lease_id: &str,
) -> Result<bool> {
    let Some(lease) = workspace_lease_by_id(pool, lease_id).await? else {
        return Ok(false);
    };

    if let Err(error) = remove_workspace_worktree_for_lease(&lease) {
        tracing::warn!(
            lease_id = %lease.lease_id,
            path = %lease.path,
            error = %error,
            "boot workspace lease reclaim could not remove workspace directory; marking lease released"
        );
    }

    complete_workspace_lease_release(pool, events, lease).await
}

async fn complete_workspace_lease_release(
    pool: &SqlitePool,
    events: &EventBus,
    lease: WorkspaceLease,
) -> Result<bool> {
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

    let removed_event = Event::WorktreeRemoved {
        wave_id: WaveId::from(lease.wave_id.clone()),
        card_id: CardId::from(lease.card_id.clone()),
        path: lease.path.clone(),
    };
    let removed_event_id = append_decision_event_in_tx(
        &mut tx,
        &PermissiveGate,
        &ActorId::KernelDispatcher,
        &scope,
        None,
        &removed_event,
    )
    .await?;
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
        id: removed_event_id,
        event_version: SYNC_EVENT_VERSION,
        actor: ActorId::KernelDispatcher,
        scope: scope.clone(),
        event: removed_event,
    });
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
        events.extend(release_workspace_lease_tx(tx, lease).await?);
    }
    Ok(events)
}

async fn release_workspace_lease_tx(
    tx: &mut Tx<'_>,
    lease: WorkspaceLease,
) -> Result<Vec<(ActorId, EventScope, Event)>> {
    remove_workspace_worktree_for_lease(&lease)?;
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
        return Ok(Vec::new());
    }
    Ok(vec![
        (
            ActorId::KernelDispatcher,
            scope.clone(),
            Event::WorktreeRemoved {
                wave_id: WaveId::from(lease.wave_id.clone()),
                card_id: CardId::from(lease.card_id.clone()),
                path: lease.path,
            },
        ),
        (
            ActorId::KernelDispatcher,
            scope,
            Event::WorkspaceReleased {
                wave_id: WaveId::from(lease.wave_id),
                card_id: CardId::from(lease.card_id),
                lease_id: lease.lease_id,
            },
        ),
    ])
}

async fn complete_workspace_lease_release_repo(
    repo: &dyn RepoEventWrite,
    lease: WorkspaceLease,
) -> Result<Vec<BroadcastEnvelope>> {
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
                return Ok(Vec::new());
            }
            let removed_event = Event::WorktreeRemoved {
                wave_id: WaveId::from(lease.wave_id.clone()),
                card_id: CardId::from(lease.card_id.clone()),
                path: lease.path.clone(),
            };
            let removed_event_id = append_decision_event_in_tx(
                tx,
                &PermissiveGate,
                &ActorId::KernelDispatcher,
                &scope,
                None,
                &removed_event,
            )
            .await?;
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
            Ok(vec![
                BroadcastEnvelope {
                    id: removed_event_id,
                    event_version: SYNC_EVENT_VERSION,
                    actor: ActorId::KernelDispatcher,
                    scope: scope.clone(),
                    event: removed_event,
                },
                BroadcastEnvelope {
                    id: event_id,
                    event_version: SYNC_EVENT_VERSION,
                    actor: ActorId::KernelDispatcher,
                    scope,
                    event,
                },
            ])
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
    #[cfg(test)]
    if take_forced_workspace_dir_remove_failure(path) {
        return Err(CalmError::Internal(format!(
            "remove workspace lease directory {}: forced removal failure for test",
            path.display()
        )));
    }
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(CalmError::Internal(format!(
            "remove workspace lease directory {}: {e}",
            path.display()
        ))),
    }
}

pub(crate) fn provision_workspace_worktree(target: &WorkspaceLeaseTarget) -> Result<()> {
    ensure_workspace_worktree_root_excluded(&target.repo_root)?;

    let parent = target.path.parent().ok_or_else(|| {
        CalmError::Internal(format!(
            "workspace lease path {} has no parent",
            target.path.display()
        ))
    })?;
    std::fs::create_dir_all(parent).map_err(|e| {
        CalmError::Internal(format!(
            "create workspace worktree parent {}: {e}",
            parent.display()
        ))
    })?;

    if git_worktree_registered(&target.repo_root, &target.path)? {
        return Ok(());
    }

    let branch_ref = format!("refs/heads/{}", target.branch);
    let branch_exists = git_ref_exists(&target.repo_root, &branch_ref)?;
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(&target.repo_root)
        .args(["worktree", "add"]);
    if branch_exists {
        command.arg(&target.path).arg(&target.branch);
    } else {
        command.args(["-b", &target.branch]).arg(&target.path);
    }
    let output = command.output().map_err(|e| {
        CalmError::Internal(format!(
            "spawn git worktree add for {}: {e}",
            target.path.display()
        ))
    })?;
    if output.status.success() || git_worktree_registered(&target.repo_root, &target.path)? {
        return Ok(());
    }
    Err(git_failed("git worktree add", &target.repo_root, &output))
}

fn remove_workspace_worktree_for_lease(lease: &WorkspaceLease) -> Result<()> {
    let Some(target) = workspace_lease_target_from_lease(lease)? else {
        // Pre-3c relative leases were never registered as git worktrees.
        return remove_workspace_dir_if_exists(&lease.path);
    };
    remove_workspace_worktree(&target)
}

pub(crate) fn remove_workspace_worktree(target: &WorkspaceLeaseTarget) -> Result<()> {
    #[cfg(test)]
    if take_forced_workspace_worktree_remove_failure(&target.path) {
        return Err(CalmError::Internal(format!(
            "remove workspace worktree {}: forced removal failure for test",
            target.path.display()
        )));
    }

    if !git_repo_available(&target.repo_root) {
        return remove_workspace_dir_if_exists(&target.path_string());
    }

    let registered = git_worktree_registered(&target.repo_root, &target.path)?;
    if registered || target.path.exists() {
        let output = Command::new("git")
            .arg("-C")
            .arg(&target.repo_root)
            .args(["worktree", "remove", "--force"])
            .arg(&target.path)
            .output()
            .map_err(|e| {
                CalmError::Internal(format!(
                    "spawn git worktree remove for {}: {e}",
                    target.path.display()
                ))
            })?;
        if !output.status.success()
            && registered
            && git_worktree_registered(&target.repo_root, &target.path)?
        {
            return Err(git_failed(
                "git worktree remove --force",
                &target.repo_root,
                &output,
            ));
        }
    }

    let branch_ref = format!("refs/heads/{}", target.branch);
    if git_ref_exists(&target.repo_root, &branch_ref)? {
        let output = Command::new("git")
            .arg("-C")
            .arg(&target.repo_root)
            .args(["branch", "-D", &target.branch])
            .output()
            .map_err(|e| {
                CalmError::Internal(format!(
                    "spawn git branch -D {} in {}: {e}",
                    target.branch,
                    target.repo_root.display()
                ))
            })?;
        if !output.status.success() && git_ref_exists(&target.repo_root, &branch_ref)? {
            return Err(git_failed("git branch -D", &target.repo_root, &output));
        }
    }

    remove_workspace_dir_if_exists(&target.path_string())
}

fn ensure_workspace_worktree_root_excluded(repo_root: &Path) -> Result<()> {
    const WORKTREE_EXCLUDE: &str = ".claude/worktrees/";
    let exclude_path = repo_root.join(".git").join("info").join("exclude");
    let existing = match std::fs::read_to_string(&exclude_path) {
        Ok(existing) => existing,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(CalmError::Internal(format!(
                "read git exclude {}: {error}",
                exclude_path.display()
            )));
        }
    };
    if existing.lines().any(|line| line.trim() == WORKTREE_EXCLUDE) {
        return Ok(());
    }
    if let Some(parent) = exclude_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            CalmError::Internal(format!(
                "create git exclude directory {}: {error}",
                parent.display()
            ))
        })?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&exclude_path)
        .map_err(|error| {
            CalmError::Internal(format!(
                "open git exclude {}: {error}",
                exclude_path.display()
            ))
        })?;
    if !existing.is_empty() && !existing.ends_with('\n') {
        file.write_all(b"\n").map_err(|error| {
            CalmError::Internal(format!(
                "write git exclude {}: {error}",
                exclude_path.display()
            ))
        })?;
    }
    file.write_all(format!("{WORKTREE_EXCLUDE}\n").as_bytes())
        .map_err(|error| {
            CalmError::Internal(format!(
                "write git exclude {}: {error}",
                exclude_path.display()
            ))
        })?;
    Ok(())
}

#[cfg(test)]
static FORCED_WORKSPACE_DIR_REMOVE_FAILURES: OnceLock<Mutex<BTreeSet<String>>> = OnceLock::new();

#[cfg(test)]
static FORCED_WORKSPACE_WORKTREE_REMOVE_FAILURES: OnceLock<Mutex<BTreeSet<String>>> =
    OnceLock::new();

#[cfg(test)]
pub(crate) fn fail_next_workspace_dir_removal_for_test(path: &str) {
    FORCED_WORKSPACE_DIR_REMOVE_FAILURES
        .get_or_init(|| Mutex::new(BTreeSet::new()))
        .lock()
        .expect("forced workspace dir removal failures lock")
        .insert(path.to_string());
}

#[cfg(test)]
fn take_forced_workspace_dir_remove_failure(path: &Path) -> bool {
    let Some(failures) = FORCED_WORKSPACE_DIR_REMOVE_FAILURES.get() else {
        return false;
    };
    failures
        .lock()
        .expect("forced workspace dir removal failures lock")
        .remove(path.to_string_lossy().as_ref())
}

#[cfg(test)]
fn fail_next_workspace_worktree_removal_for_test(path: &Path) {
    FORCED_WORKSPACE_WORKTREE_REMOVE_FAILURES
        .get_or_init(|| Mutex::new(BTreeSet::new()))
        .lock()
        .expect("forced workspace worktree removal failures lock")
        .insert(path.to_string_lossy().to_string());
}

#[cfg(test)]
fn take_forced_workspace_worktree_remove_failure(path: &Path) -> bool {
    let Some(failures) = FORCED_WORKSPACE_WORKTREE_REMOVE_FAILURES.get() else {
        return false;
    };
    failures
        .lock()
        .expect("forced workspace worktree removal failures lock")
        .remove(path.to_string_lossy().as_ref())
}

pub(crate) fn workspace_lease_path_for(
    repo_root: &Path,
    wave_id: &str,
    card_id: &str,
) -> Result<PathBuf> {
    validate_path_segment("wave_id", wave_id)?;
    validate_path_segment("card_id", card_id)?;
    if !repo_root.is_absolute() {
        return Err(CalmError::BadRequest(format!(
            "workspace lease repo root must be absolute: {}",
            repo_root.display()
        )));
    }
    Ok(repo_root
        .join(".claude")
        .join("worktrees")
        .join(wave_id)
        .join(card_id))
}

pub(crate) fn plain_workspace_lease_path_for(wave_id: &str, card_id: &str) -> Result<PathBuf> {
    validate_path_segment("wave_id", wave_id)?;
    validate_path_segment("card_id", card_id)?;
    Ok(PathBuf::from(".claude")
        .join("worktrees")
        .join(wave_id)
        .join(card_id))
}

pub(crate) fn workspace_slice_branch_for(wave_id: &str, card_id: &str) -> Result<String> {
    validate_path_segment("wave_id", wave_id)?;
    validate_path_segment("card_id", card_id)?;
    Ok(format!("neige/{wave_id}/{card_id}"))
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

fn git_repo_root_for_wave_cwd(wave_id: &str, cwd: &str) -> Result<PathBuf> {
    let cwd_path = Path::new(cwd);
    if cwd.trim().is_empty() || !cwd_path.is_absolute() {
        return Err(CalmError::BadRequest(format!(
            "wave {wave_id} cwd must be an absolute git repository path for workspace leasing"
        )));
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd_path)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| {
            CalmError::Internal(format!(
                "spawn git rev-parse --show-toplevel for wave {wave_id} cwd {}: {e}",
                cwd_path.display()
            ))
        })?;
    if !output.status.success() {
        return Err(CalmError::BadRequest(format!(
            "wave {wave_id} cwd {} is not a git repository: {}",
            cwd_path.display(),
            output_summary(&output)
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let repo_root = stdout.trim_end_matches(&['\r', '\n'][..]);
    if repo_root.is_empty() {
        return Err(CalmError::BadRequest(format!(
            "wave {wave_id} cwd {} did not resolve to a git repository root",
            cwd_path.display()
        )));
    }
    let repo_root = PathBuf::from(repo_root);
    if !repo_root.is_absolute() {
        return Err(CalmError::BadRequest(format!(
            "wave {wave_id} git repository root must be absolute: {}",
            repo_root.display()
        )));
    }
    Ok(repo_root)
}

fn workspace_lease_target_from_lease(
    lease: &WorkspaceLease,
) -> Result<Option<WorkspaceLeaseTarget>> {
    validate_path_segment("wave_id", &lease.wave_id)?;
    validate_path_segment("card_id", &lease.card_id)?;
    let path = PathBuf::from(&lease.path);
    if !path.is_absolute() {
        return Ok(None);
    }
    let Some(card_dir) = path.file_name().and_then(|s| s.to_str()) else {
        return Ok(None);
    };
    let Some(wave_dir_path) = path.parent() else {
        return Ok(None);
    };
    let Some(wave_dir) = wave_dir_path.file_name().and_then(|s| s.to_str()) else {
        return Ok(None);
    };
    let Some(worktrees_path) = wave_dir_path.parent() else {
        return Ok(None);
    };
    let Some(worktrees_dir) = worktrees_path.file_name().and_then(|s| s.to_str()) else {
        return Ok(None);
    };
    let Some(claude_path) = worktrees_path.parent() else {
        return Ok(None);
    };
    let Some(claude_dir) = claude_path.file_name().and_then(|s| s.to_str()) else {
        return Ok(None);
    };
    let Some(repo_root) = claude_path.parent() else {
        return Ok(None);
    };
    if card_dir != lease.card_id
        || wave_dir != lease.wave_id
        || worktrees_dir != "worktrees"
        || claude_dir != ".claude"
        || !repo_root.is_absolute()
    {
        return Ok(None);
    }
    Ok(Some(WorkspaceLeaseTarget {
        repo_root: repo_root.to_path_buf(),
        path,
        branch: workspace_slice_branch_for(&lease.wave_id, &lease.card_id)?,
    }))
}

fn git_repo_available(repo_root: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["rev-parse", "--git-dir"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn git_ref_exists(repo_root: &Path, full_ref: &str) -> Result<bool> {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["show-ref", "--verify", "--quiet", full_ref])
        .status()
        .map_err(|e| {
            CalmError::Internal(format!(
                "spawn git show-ref {full_ref} in {}: {e}",
                repo_root.display()
            ))
        })?;
    Ok(status.success())
}

fn git_worktree_registered(repo_root: &Path, path: &Path) -> Result<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .map_err(|e| {
            CalmError::Internal(format!(
                "spawn git worktree list in {}: {e}",
                repo_root.display()
            ))
        })?;
    if !output.status.success() {
        return Err(git_failed("git worktree list", repo_root, &output));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.lines().any(|line| {
        line.strip_prefix("worktree ")
            .map(|listed| Path::new(listed) == path)
            .unwrap_or(false)
    }))
}

fn git_failed(action: &str, repo_root: &Path, output: &Output) -> CalmError {
    CalmError::Internal(format!(
        "{action} failed in {}: {}",
        repo_root.display(),
        output_summary(output)
    ))
}

fn output_summary(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !stderr.is_empty() {
        return stderr;
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !stdout.is_empty() {
        return stdout;
    }
    format!("exit status {}", output.status)
}

const WORKSPACE_LEASE_MS: TimestampMs = 60_000;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite::begin_immediate_tx;

    #[test]
    fn remove_workspace_dir_if_exists_treats_missing_as_success() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("already-gone");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::remove_dir_all(&path).unwrap();

        remove_workspace_dir_if_exists(path.to_str().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn acquire_workspace_lease_anchors_under_git_root_without_creating_leaf() {
        let tmp = tempfile::tempdir().unwrap();
        init_git_repo(tmp.path());
        let (repo, wave_id, card_id) = lease_fixture(tmp.path()).await;

        let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
        let target = prepare_workspace_lease_target_tx(&mut tx, &wave_id, &card_id)
            .await
            .unwrap();
        assert!(target.repo_root.is_absolute());
        assert_eq!(
            target.repo_root.canonicalize().unwrap(),
            tmp.path().canonicalize().unwrap()
        );
        assert!(target.path.is_absolute());
        assert!(target.path.starts_with(&target.repo_root));

        let (lease, _event) =
            acquire_workspace_lease_tx(&mut tx, &card_id, &wave_id, "op-test", &target)
                .await
                .unwrap();
        assert_eq!(lease.path, target.path_string());
        assert!(
            target.path.parent().unwrap().is_dir(),
            "lease acquisition creates the worktree parent"
        );
        assert!(
            !target.path.exists(),
            "lease acquisition must leave the worktree leaf for git worktree add"
        );
        tx.rollback().await.unwrap();
    }

    #[tokio::test]
    async fn workspace_lease_target_rejects_non_git_wave_cwd_without_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let (repo, wave_id, card_id) = lease_fixture(tmp.path()).await;

        let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
        let err = prepare_workspace_lease_target_tx(&mut tx, &wave_id, &card_id)
            .await
            .unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
        tx.rollback().await.unwrap();

        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workspace_leases")
            .fetch_one(repo.pool())
            .await
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[tokio::test]
    async fn acquire_plain_workspace_lease_creates_leaf_for_non_git_wave_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let (repo, wave_id, card_id) = lease_fixture(tmp.path()).await;
        let path = plain_workspace_lease_path_for(&wave_id, &card_id).unwrap();
        assert!(
            !path.is_absolute(),
            "plain workspace lease path is legacy-relative"
        );

        let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
        let (lease, _event) =
            acquire_plain_workspace_lease_tx(&mut tx, &card_id, &wave_id, "op-test", &path)
                .await
                .unwrap();
        tx.commit().await.unwrap();

        assert_eq!(lease.path, path.to_string_lossy().to_string());
        assert!(path.is_dir(), "plain lease acquisition creates the leaf");

        let events = EventBus::new();
        assert!(
            release_workspace_lease_for_card_repo(&repo, &events, &card_id)
                .await
                .unwrap()
        );
        assert!(!path.exists(), "plain lease release removes the leaf");
    }

    #[test]
    fn workspace_worktree_remove_deletes_branch_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        init_git_repo(tmp.path());
        let target = WorkspaceLeaseTarget {
            repo_root: tmp.path().to_path_buf(),
            path: tmp.path().join(".claude/worktrees/wave-a/card-a"),
            branch: workspace_slice_branch_for("wave-a", "card-a").unwrap(),
        };

        provision_workspace_worktree(&target).unwrap();
        assert!(target.path.is_dir(), "provisioned worktree exists");
        assert!(
            git_ref_exists(&target.repo_root, &format!("refs/heads/{}", target.branch)).unwrap(),
            "slice branch exists"
        );

        remove_workspace_worktree(&target).unwrap();
        assert!(!target.path.exists(), "worktree path removed");
        assert!(
            !git_ref_exists(&target.repo_root, &format!("refs/heads/{}", target.branch)).unwrap(),
            "slice branch removed"
        );

        remove_workspace_worktree(&target).unwrap();
    }

    #[test]
    fn workspace_worktree_provision_excludes_root_from_base_status() {
        let tmp = tempfile::tempdir().unwrap();
        init_git_repo(tmp.path());
        let target = WorkspaceLeaseTarget {
            repo_root: tmp.path().to_path_buf(),
            path: tmp.path().join(".claude/worktrees/wave-clean/card-clean"),
            branch: workspace_slice_branch_for("wave-clean", "card-clean").unwrap(),
        };

        provision_workspace_worktree(&target).unwrap();

        let status = git_stdout(tmp.path(), ["status", "--short", "--untracked-files=all"]);
        assert_eq!(status, "", "base repo must stay clean after provisioning");

        provision_workspace_worktree(&target).unwrap();
        let exclude = std::fs::read_to_string(tmp.path().join(".git/info/exclude")).unwrap();
        assert_eq!(
            exclude
                .lines()
                .filter(|line| line.trim() == ".claude/worktrees/")
                .count(),
            1,
            "worktree exclude entry is idempotent"
        );
    }

    #[tokio::test]
    async fn card_release_marks_released_when_worktree_remove_fails() {
        let tmp = tempfile::tempdir().unwrap();
        init_git_repo(tmp.path());
        let (repo, wave_id, card_id) = lease_fixture(tmp.path()).await;

        let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
        let target = prepare_workspace_lease_target_tx(&mut tx, &wave_id, &card_id)
            .await
            .unwrap();
        let (lease, _event) =
            acquire_workspace_lease_tx(&mut tx, &card_id, &wave_id, "op-test", &target)
                .await
                .unwrap();
        tx.commit().await.unwrap();
        provision_workspace_worktree(&target).unwrap();
        fail_next_workspace_worktree_removal_for_test(&target.path);

        let events = EventBus::new();
        assert!(
            release_workspace_lease_for_card_repo(&repo, &events, &card_id)
                .await
                .unwrap()
        );

        let state: String =
            sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id = ?1")
                .bind(&lease.lease_id)
                .fetch_one(repo.pool())
                .await
                .unwrap();
        assert_eq!(state, "released");
    }

    async fn lease_fixture(wave_cwd: &Path) -> (crate::db::sqlite::SqlxRepo, String, String) {
        let repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap();
        let cove = crate::db::RepoSyncDomainRaw::cove_create(
            &repo,
            crate::model::NewCove {
                name: "lease fixture".into(),
                color: "#101010".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
        let wave = crate::db::RepoSyncDomainRaw::wave_create(
            &repo,
            crate::model::NewWave {
                cove_id: cove.id,
                title: "lease fixture".into(),
                sort: None,
                cwd: wave_cwd.display().to_string(),
                attach_folder: false,
                theme: crate::routes::theme::RequestTheme::default_dark(),
            },
        )
        .await
        .unwrap();
        let card = crate::db::RepoSyncDomainRaw::card_create(
            &repo,
            crate::model::NewCard {
                wave_id: wave.id.clone(),
                kind: "codex".into(),
                sort: None,
                payload: serde_json::Value::Null,
            },
        )
        .await
        .unwrap();
        (repo, wave.id.to_string(), card.id.to_string())
    }

    fn init_git_repo(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        run_git(path, ["init"]);
        run_git(path, ["config", "user.email", "lease@example.test"]);
        run_git(path, ["config", "user.name", "Lease Test"]);
        std::fs::write(path.join("README.md"), "initial\n").unwrap();
        run_git(path, ["add", "README.md"]);
        run_git(path, ["commit", "-m", "initial"]);
    }

    fn run_git<const N: usize>(repo: &Path, args: [&str; N]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed in {}\nstdout:\n{}\nstderr:\n{}",
            args,
            repo.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_stdout<const N: usize>(repo: &Path, args: [&str; N]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed in {}\nstdout:\n{}\nstderr:\n{}",
            args,
            repo.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).to_string()
    }
}
