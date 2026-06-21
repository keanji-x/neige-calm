use std::{
    collections::BTreeSet,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Output},
};

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
    let removed = remove_workspace_worktree_for_lease(&lease)?;
    if removed {
        persist_worktree_removed_for_lease(pool, events, &lease).await?;
    }
    complete_workspace_lease_release(pool, events, lease).await
}

pub(crate) async fn remove_workspace_artifact_for_lease_by_id(
    pool: &SqlitePool,
    events: &EventBus,
    lease_id: &str,
) -> Result<bool> {
    let Some(lease) = workspace_lease_by_id(pool, lease_id).await? else {
        return Ok(false);
    };
    let removed = remove_workspace_worktree_for_lease(&lease)?;
    if removed {
        persist_worktree_removed_for_lease(pool, events, &lease).await?;
    }
    Ok(removed)
}

pub(crate) async fn release_workspace_lease_for_card_repo(
    repo: &dyn RepoEventWrite,
    events: &EventBus,
    card_id: &str,
) -> Result<bool> {
    let card_id = card_id.to_string();
    let envelopes = write_in_tx_typed(repo, move |tx| {
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
                return Ok(Vec::new());
            };
            let lease = row_to_workspace_lease(row)?;
            let events = release_workspace_lease_tx(tx, lease).await?;
            append_workspace_events_tx(tx, events).await
        })
    })
    .await?;
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

async fn release_workspace_lease_on_boot(
    pool: &SqlitePool,
    events: &EventBus,
    lease_id: &str,
) -> Result<bool> {
    let Some(lease) = workspace_lease_by_id(pool, lease_id).await? else {
        return Ok(false);
    };

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

    let mut envelopes = Vec::new();
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
    envelopes.push(BroadcastEnvelope {
        id: event_id,
        event_version: SYNC_EVENT_VERSION,
        actor: ActorId::KernelDispatcher,
        scope,
        event,
    });
    tx.commit().await?;

    for envelope in envelopes {
        events.emit_envelope(envelope);
    }
    Ok(true)
}

pub(crate) async fn release_workspace_leases_for_wave_tx(
    tx: &mut Tx<'_>,
    wave_id: &str,
) -> Result<WorkspaceWaveRelease> {
    let sweep = workspace_wave_sweep_for_wave_tx(tx, wave_id).await;
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
    Ok(WorkspaceWaveRelease { events, sweep })
}

async fn release_workspace_lease_tx(
    tx: &mut Tx<'_>,
    lease: WorkspaceLease,
) -> Result<Vec<(ActorId, EventScope, Event)>> {
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
    Ok(vec![(
        ActorId::KernelDispatcher,
        scope,
        Event::WorkspaceReleased {
            wave_id: WaveId::from(lease.wave_id),
            card_id: CardId::from(lease.card_id),
            lease_id: lease.lease_id,
        },
    )])
}

#[derive(Clone, Debug)]
pub(crate) struct WorkspaceWaveRelease {
    pub(crate) events: Vec<(ActorId, EventScope, Event)>,
    pub(crate) sweep: Option<WorkspaceWaveSweep>,
}

#[derive(Clone, Debug)]
pub(crate) struct WorkspaceWaveSweep {
    wave_id: String,
    cove_id: String,
    cwd: String,
    leases: Vec<WorkspaceLease>,
}

async fn workspace_wave_sweep_for_wave_tx(
    tx: &mut Tx<'_>,
    wave_id: &str,
) -> Option<WorkspaceWaveSweep> {
    if let Err(error) = validate_path_segment("wave_id", wave_id) {
        tracing::warn!(
            wave_id,
            error = %error,
            "workspace wave teardown skipped preserved worktree sweep for invalid wave id"
        );
        return None;
    }
    let row = match sqlx::query("SELECT cwd, cove_id FROM waves WHERE id = ?1")
        .bind(wave_id)
        .fetch_optional(&mut **tx)
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => return None,
        Err(error) => {
            tracing::warn!(
                wave_id,
                error = %error,
                "workspace wave teardown could not read cwd for preserved worktree sweep"
            );
            return None;
        }
    };
    let cwd: String = match row.try_get("cwd") {
        Ok(cwd) => cwd,
        Err(error) => {
            tracing::warn!(
                wave_id,
                error = %error,
                "workspace wave teardown could not read cwd column for preserved worktree sweep"
            );
            return None;
        }
    };
    let cove_id: String = match row.try_get("cove_id") {
        Ok(cove_id) => cove_id,
        Err(error) => {
            tracing::warn!(
                wave_id,
                error = %error,
                "workspace wave teardown could not read cove_id column for preserved worktree sweep"
            );
            return None;
        }
    };
    let leases = match sqlx::query(
        r#"SELECT lease_id, card_id, wave_id, path, state, boot_id
           FROM workspace_leases
           WHERE wave_id = ?1
           ORDER BY created_at_ms ASC, lease_id ASC"#,
    )
    .bind(wave_id)
    .fetch_all(&mut **tx)
    .await
    {
        Ok(rows) => rows
            .into_iter()
            .filter_map(|row| match row_to_workspace_lease(row) {
                Ok(lease) => Some(lease),
                Err(error) => {
                    tracing::warn!(
                        wave_id,
                        error = %error,
                        "workspace wave teardown skipped unparseable persisted lease row"
                    );
                    None
                }
            })
            .collect(),
        Err(error) => {
            tracing::warn!(
                wave_id,
                error = %error,
                "workspace wave teardown could not read persisted lease paths for sweep"
            );
            Vec::new()
        }
    };
    Some(WorkspaceWaveSweep {
        wave_id: wave_id.to_string(),
        cove_id,
        cwd,
        leases,
    })
}

#[derive(Clone, Debug)]
struct RemovedWorkspaceWorktree {
    card_id: String,
    path: String,
}

pub(crate) async fn sweep_workspace_worktrees_for_wave_repo(
    repo: &dyn RepoEventWrite,
    events: &EventBus,
    sweep: WorkspaceWaveSweep,
) -> Result<usize> {
    let repo_roots = repo_roots_for_wave_sweep(&sweep);
    if repo_roots.is_empty() {
        return Ok(0);
    }
    let mut removed = Vec::new();
    for repo_root in repo_roots {
        removed.extend(sweep_workspace_worktree_root_for_wave(
            &repo_root,
            &sweep.wave_id,
        ));
        sweep_workspace_slice_branches_for_wave(&repo_root, &sweep.wave_id);
    }
    let removed_count = removed.len();
    if removed.is_empty() {
        return Ok(0);
    }
    let envelopes = persist_wave_sweep_removed_events(repo, &sweep, removed).await?;
    for envelope in envelopes {
        events.emit_envelope(envelope);
    }
    Ok(removed_count)
}

fn repo_roots_for_wave_sweep(sweep: &WorkspaceWaveSweep) -> Vec<PathBuf> {
    let mut roots = BTreeSet::new();
    for lease in &sweep.leases {
        match workspace_lease_target_from_lease(lease) {
            Ok(Some(target)) => {
                roots.insert(target.repo_root);
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    wave_id = %sweep.wave_id,
                    lease_id = %lease.lease_id,
                    path = %lease.path,
                    error = %error,
                    "workspace wave teardown skipped invalid persisted lease path"
                );
            }
        }
    }
    if !roots.is_empty() {
        return roots.into_iter().collect();
    }
    match git_repo_root_for_wave_cwd(&sweep.wave_id, &sweep.cwd) {
        Ok(repo_root) => vec![repo_root],
        Err(error) => {
            tracing::error!(
                wave_id = %sweep.wave_id,
                cwd = %sweep.cwd,
                error = %error,
                "workspace wave teardown could not derive repo root from persisted lease paths or wave cwd"
            );
            Vec::new()
        }
    }
}

pub(crate) async fn sweep_workspace_worktrees_for_waves_repo(
    repo: &dyn RepoEventWrite,
    events: &EventBus,
    sweeps: Vec<WorkspaceWaveSweep>,
) -> Result<usize> {
    let mut removed = 0;
    for sweep in sweeps {
        removed += sweep_workspace_worktrees_for_wave_repo(repo, events, sweep).await?;
    }
    Ok(removed)
}

fn sweep_workspace_worktree_root_for_wave(
    repo_root: &Path,
    wave_id: &str,
) -> Vec<RemovedWorkspaceWorktree> {
    let mut removed = Vec::new();
    let wave_root = repo_root.join(".claude").join("worktrees").join(wave_id);
    let entries = match std::fs::read_dir(&wave_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return removed,
        Err(error) => {
            tracing::warn!(
                repo_root = %repo_root.display(),
                wave_id,
                path = %wave_root.display(),
                error = %error,
                "workspace wave teardown could not read preserved worktree root"
            );
            return removed;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!(
                    repo_root = %repo_root.display(),
                    wave_id,
                    error = %error,
                    "workspace wave teardown could not read preserved worktree entry"
                );
                continue;
            }
        };
        let path = entry.path();
        let Some(parts) = workspace_lease_path_parts(&path) else {
            tracing::warn!(
                repo_root = %repo_root.display(),
                wave_id,
                path = %path.display(),
                "workspace wave teardown skipped non-lease-shaped preserved worktree path"
            );
            continue;
        };
        if parts.repo_root.as_path() != repo_root || parts.wave_id != wave_id {
            tracing::warn!(
                repo_root = %repo_root.display(),
                wave_id,
                path = %path.display(),
                "workspace wave teardown skipped preserved worktree outside wave root"
            );
            continue;
        }
        if let Err(error) = validate_path_segment("card_id", &parts.card_id) {
            tracing::warn!(
                repo_root = %repo_root.display(),
                wave_id,
                card_id = %parts.card_id,
                path = %path.display(),
                error = %error,
                "workspace wave teardown skipped preserved worktree with invalid card id"
            );
            continue;
        }
        let target = WorkspaceLeaseTarget {
            repo_root: repo_root.to_path_buf(),
            path,
            branch: match workspace_slice_branch_for(wave_id, &parts.card_id) {
                Ok(branch) => branch,
                Err(error) => {
                    tracing::warn!(
                        repo_root = %repo_root.display(),
                        wave_id,
                        card_id = %parts.card_id,
                        error = %error,
                        "workspace wave teardown skipped preserved worktree branch derivation"
                    );
                    continue;
                }
            },
        };
        match remove_workspace_worktree(&target) {
            Ok(true) => removed.push(RemovedWorkspaceWorktree {
                card_id: parts.card_id,
                path: target.path_string(),
            }),
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(
                    repo_root = %repo_root.display(),
                    wave_id,
                    card_id = %parts.card_id,
                    path = %target.path.display(),
                    error = %error,
                    "workspace wave teardown could not remove preserved worktree"
                );
            }
        }
    }
    match std::fs::remove_dir(&wave_root) {
        Ok(()) => {}
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
            ) => {}
        Err(error) => {
            tracing::warn!(
                repo_root = %repo_root.display(),
                wave_id,
                path = %wave_root.display(),
                error = %error,
                "workspace wave teardown could not remove empty preserved worktree root"
            );
        }
    }
    removed
}

fn sweep_workspace_slice_branches_for_wave(repo_root: &Path, wave_id: &str) {
    let branch_prefix = format!("neige/{wave_id}/");
    let ref_prefix = format!("refs/heads/neige/{wave_id}");
    let output = match Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["for-each-ref", "--format=%(refname:short)", &ref_prefix])
        .output()
    {
        Ok(output) => output,
        Err(error) => {
            tracing::warn!(
                repo_root = %repo_root.display(),
                wave_id,
                error = %error,
                "workspace wave teardown could not list preserved slice branches"
            );
            return;
        }
    };
    if !output.status.success() {
        tracing::warn!(
            repo_root = %repo_root.display(),
            wave_id,
            error = %git_failed("git for-each-ref", repo_root, &output),
            "workspace wave teardown could not list preserved slice branches"
        );
        return;
    }
    for branch in String::from_utf8_lossy(&output.stdout).lines() {
        let Some(card_id) = branch.strip_prefix(&branch_prefix) else {
            continue;
        };
        if validate_path_segment("card_id", card_id).is_err() {
            continue;
        }
        let branch_ref = format!("refs/heads/{branch}");
        let delete = Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["branch", "-D", branch])
            .output();
        let output = match delete {
            Ok(output) => output,
            Err(error) => {
                tracing::warn!(
                    repo_root = %repo_root.display(),
                    wave_id,
                    branch,
                    error = %error,
                    "workspace wave teardown could not spawn preserved branch delete"
                );
                continue;
            }
        };
        match git_ref_exists(repo_root, &branch_ref) {
            Ok(true) if !output.status.success() => {
                tracing::warn!(
                    repo_root = %repo_root.display(),
                    wave_id,
                    branch,
                    error = %git_failed("git branch -D", repo_root, &output),
                    "workspace wave teardown could not delete preserved slice branch"
                );
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(
                    repo_root = %repo_root.display(),
                    wave_id,
                    branch,
                    error = %error,
                    "workspace wave teardown could not verify preserved slice branch deletion"
                );
            }
        }
    }
}

async fn persist_wave_sweep_removed_events(
    repo: &dyn RepoEventWrite,
    sweep: &WorkspaceWaveSweep,
    removed: Vec<RemovedWorkspaceWorktree>,
) -> Result<Vec<BroadcastEnvelope>> {
    let wave_id = sweep.wave_id.clone();
    let cove_id = sweep.cove_id.clone();
    write_in_tx_typed(repo, move |tx| {
        let wave_id = wave_id.clone();
        let cove_id = cove_id.clone();
        let removed = removed.clone();
        Box::pin(async move {
            let mut events = Vec::with_capacity(removed.len());
            for removed in removed {
                let scope = EventScope::Card {
                    card: CardId::from(removed.card_id.clone()),
                    wave: WaveId::from(wave_id.clone()),
                    cove: CoveId::from(cove_id.clone()),
                };
                events.push((
                    ActorId::KernelDispatcher,
                    scope,
                    Event::WorktreeRemoved {
                        wave_id: WaveId::from(wave_id.clone()),
                        card_id: CardId::from(removed.card_id),
                        path: removed.path,
                    },
                ));
            }
            append_workspace_events_tx(tx, events).await
        })
    })
    .await
}

async fn persist_worktree_removed_for_lease(
    pool: &SqlitePool,
    events: &EventBus,
    lease: &WorkspaceLease,
) -> Result<()> {
    let mut tx = begin_immediate_tx(pool).await?;
    let scope = workspace_scope_tx(&mut tx, &lease.card_id, &lease.wave_id).await?;
    let event = Event::WorktreeRemoved {
        wave_id: WaveId::from(lease.wave_id.clone()),
        card_id: CardId::from(lease.card_id.clone()),
        path: lease.path.clone(),
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
    Ok(())
}

async fn append_workspace_events_tx(
    tx: &mut Tx<'_>,
    events: Vec<(ActorId, EventScope, Event)>,
) -> Result<Vec<BroadcastEnvelope>> {
    let mut envelopes = Vec::with_capacity(events.len());
    for (actor, scope, event) in events {
        let event_id =
            append_decision_event_in_tx(tx, &PermissiveGate, &actor, &scope, None, &event).await?;
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

fn remove_workspace_dir_if_exists(path: &str) -> Result<bool> {
    let path = Path::new(path);
    let existed = path.exists();
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(existed),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
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

    match git_worktree_registration(&target.repo_root, &target.path)? {
        GitWorktreeRegistration::Present if target.path.is_dir() => return Ok(()),
        GitWorktreeRegistration::Present | GitWorktreeRegistration::Prunable => {
            prune_stale_workspace_worktree_registration(target)?;
        }
        GitWorktreeRegistration::Absent => {}
    }

    clear_stale_unregistered_workspace_dir_before_add(target)?;

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
    if output.status.success() {
        if target.path.is_dir() {
            return Ok(());
        }
        return Err(CalmError::Internal(format!(
            "git worktree add for {} succeeded but the worktree directory is missing",
            target.path.display()
        )));
    }
    if git_worktree_ready(&target.repo_root, &target.path)? {
        return Ok(());
    }
    Err(git_failed("git worktree add", &target.repo_root, &output))
}

fn clear_stale_unregistered_workspace_dir_before_add(target: &WorkspaceLeaseTarget) -> Result<()> {
    if !workspace_dir_is_non_empty(&target.path)? {
        return Ok(());
    }
    ensure_lease_owned_worktree_target(target)?;
    remove_workspace_dir_if_exists(&target.path_string())?;
    prune_stale_workspace_worktree_registration(target)?;
    Ok(())
}

fn workspace_dir_is_non_empty(path: &Path) -> Result<bool> {
    if !path.is_dir() {
        return Ok(false);
    }
    let mut entries = std::fs::read_dir(path).map_err(|e| {
        CalmError::Internal(format!(
            "read workspace worktree directory {}: {e}",
            path.display()
        ))
    })?;
    match entries.next() {
        Some(Ok(_)) => Ok(true),
        Some(Err(e)) => Err(CalmError::Internal(format!(
            "read workspace worktree directory {}: {e}",
            path.display()
        ))),
        None => Ok(false),
    }
}

fn remove_workspace_worktree_for_lease(lease: &WorkspaceLease) -> Result<bool> {
    let Some(target) = workspace_lease_target_from_lease(lease)? else {
        // Pre-3c relative leases were never registered as git worktrees.
        return remove_workspace_dir_if_exists(&lease.path);
    };
    remove_workspace_worktree(&target)
}

pub(crate) fn remove_workspace_worktree(target: &WorkspaceLeaseTarget) -> Result<bool> {
    if !git_repo_available(&target.repo_root) {
        return remove_workspace_dir_if_exists(&target.path_string());
    }

    let registered = git_worktree_registered(&target.repo_root, &target.path)?;
    let path_existed = target.path.exists();
    if registered || path_existed {
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
    let branch_existed = git_ref_exists(&target.repo_root, &branch_ref)?;
    if branch_existed {
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

    let dir_removed = remove_workspace_dir_if_exists(&target.path_string())?;
    Ok(registered || path_existed || branch_existed || dir_removed)
}

fn ensure_workspace_worktree_root_excluded(repo_root: &Path) -> Result<()> {
    const WORKTREE_EXCLUDE: &str = ".claude/worktrees/";
    let exclude_path = git_exclude_path(repo_root)?;
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

fn git_exclude_path(repo_root: &Path) -> Result<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["rev-parse", "--git-path", "info/exclude"])
        .output()
        .map_err(|e| {
            CalmError::Internal(format!(
                "spawn git rev-parse --git-path info/exclude in {}: {e}",
                repo_root.display()
            ))
        })?;
    if !output.status.success() {
        return Err(git_failed(
            "git rev-parse --git-path info/exclude",
            repo_root,
            &output,
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let exclude_path = stdout.trim_end_matches(&['\r', '\n'][..]);
    if exclude_path.is_empty() {
        return Err(CalmError::Internal(format!(
            "git rev-parse --git-path info/exclude in {} returned an empty path",
            repo_root.display()
        )));
    }
    let exclude_path = PathBuf::from(exclude_path);
    if exclude_path.is_absolute() {
        Ok(exclude_path)
    } else {
        Ok(repo_root.join(exclude_path))
    }
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
    let Some(parts) = workspace_lease_path_parts(&path) else {
        return Ok(None);
    };
    if parts.card_id != lease.card_id || parts.wave_id != lease.wave_id {
        return Ok(None);
    }
    Ok(Some(WorkspaceLeaseTarget {
        repo_root: parts.repo_root,
        path,
        branch: workspace_slice_branch_for(&lease.wave_id, &lease.card_id)?,
    }))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkspaceLeasePathParts {
    repo_root: PathBuf,
    wave_id: String,
    card_id: String,
}

fn workspace_lease_path_parts(path: &Path) -> Option<WorkspaceLeasePathParts> {
    if !path.is_absolute() {
        return None;
    }
    let card_id = path.file_name()?.to_str()?;
    let wave_path = path.parent()?;
    let wave_id = wave_path.file_name()?.to_str()?;
    let worktrees_path = wave_path.parent()?;
    let worktrees_dir = worktrees_path.file_name()?.to_str()?;
    let claude_path = worktrees_path.parent()?;
    let claude_dir = claude_path.file_name()?.to_str()?;
    let repo_root = claude_path.parent()?;
    if worktrees_dir != "worktrees" || claude_dir != ".claude" || !repo_root.is_absolute() {
        return None;
    }
    Some(WorkspaceLeasePathParts {
        repo_root: repo_root.to_path_buf(),
        wave_id: wave_id.to_string(),
        card_id: card_id.to_string(),
    })
}

fn ensure_lease_owned_worktree_target(target: &WorkspaceLeaseTarget) -> Result<()> {
    let Some(parts) = workspace_lease_path_parts(&target.path) else {
        return Err(CalmError::Internal(format!(
            "refusing to clear non-lease workspace worktree path {}",
            target.path.display()
        )));
    };
    validate_path_segment("wave_id", &parts.wave_id)?;
    validate_path_segment("card_id", &parts.card_id)?;
    if parts.repo_root.as_path() != target.repo_root.as_path() {
        return Err(CalmError::Internal(format!(
            "refusing to clear workspace worktree path {} outside repo root {}",
            target.path.display(),
            target.repo_root.display()
        )));
    }
    let expected_branch = workspace_slice_branch_for(&parts.wave_id, &parts.card_id)?;
    if target.branch != expected_branch {
        return Err(CalmError::Internal(format!(
            "refusing to clear workspace worktree path {} for unexpected branch {}",
            target.path.display(),
            target.branch
        )));
    }
    Ok(())
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GitWorktreeRegistration {
    Absent,
    Present,
    Prunable,
}

fn git_worktree_registered(repo_root: &Path, path: &Path) -> Result<bool> {
    Ok(git_worktree_registration(repo_root, path)? != GitWorktreeRegistration::Absent)
}

fn git_worktree_ready(repo_root: &Path, path: &Path) -> Result<bool> {
    Ok(
        git_worktree_registration(repo_root, path)? == GitWorktreeRegistration::Present
            && path.is_dir(),
    )
}

fn git_worktree_registration(repo_root: &Path, path: &Path) -> Result<GitWorktreeRegistration> {
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
    let mut current_matches = false;
    let mut current_prunable = false;
    for line in stdout.lines() {
        if let Some(listed) = line.strip_prefix("worktree ") {
            if current_matches {
                return Ok(if current_prunable {
                    GitWorktreeRegistration::Prunable
                } else {
                    GitWorktreeRegistration::Present
                });
            }
            current_matches = Path::new(listed) == path;
            current_prunable = false;
        } else if current_matches && (line == "prunable" || line.starts_with("prunable ")) {
            current_prunable = true;
        }
    }
    if current_matches {
        return Ok(if current_prunable {
            GitWorktreeRegistration::Prunable
        } else {
            GitWorktreeRegistration::Present
        });
    }
    Ok(GitWorktreeRegistration::Absent)
}

fn prune_stale_workspace_worktree_registration(target: &WorkspaceLeaseTarget) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(&target.repo_root)
        .args(["worktree", "prune", "--expire", "now"])
        .output()
        .map_err(|e| {
            CalmError::Internal(format!(
                "spawn git worktree prune in {}: {e}",
                target.repo_root.display()
            ))
        })?;
    if !output.status.success() {
        return Err(git_failed("git worktree prune", &target.repo_root, &output));
    }
    if git_worktree_registered(&target.repo_root, &target.path)? {
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
        if !output.status.success() && git_worktree_registered(&target.repo_root, &target.path)? {
            return Err(git_failed(
                "git worktree remove --force",
                &target.repo_root,
                &output,
            ));
        }
    }
    Ok(())
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
    async fn worktree_mode_workspace_leased_is_not_ready_until_worktree_provisioned() {
        let tmp = tempfile::tempdir().unwrap();
        init_git_repo(tmp.path());
        let (repo, wave_id, card_id) = lease_fixture(tmp.path()).await;

        let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
        let target = prepare_workspace_lease_target_tx(&mut tx, &wave_id, &card_id)
            .await
            .unwrap();
        let (lease, leased) =
            acquire_workspace_lease_tx(&mut tx, &card_id, &wave_id, "op-test", &target)
                .await
                .unwrap();
        tx.commit().await.unwrap();

        assert!(matches!(leased.event, Event::WorkspaceLeased { .. }));
        assert_eq!(lease.path, target.path_string());
        assert!(
            !Path::new(&lease.path).exists(),
            "workspace.leased carries the future worktree leaf, not a usable cwd"
        );
        assert_eq!(event_kind_count(&repo, "workspace.leased").await, 1);
        assert_eq!(event_kind_count(&repo, "worktree.provisioned").await, 0);

        provision_workspace_worktree(&target).unwrap();
        let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
        let scope = workspace_scope_tx(&mut tx, &card_id, &wave_id)
            .await
            .unwrap();
        append_workspace_events_tx(
            &mut tx,
            vec![(
                ActorId::KernelDispatcher,
                scope,
                Event::WorktreeProvisioned {
                    wave_id: WaveId::from(wave_id.clone()),
                    card_id: CardId::from(card_id.clone()),
                    path: target.path_string(),
                },
            )],
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        assert!(
            target.path.is_dir(),
            "worktree.provisioned is the ready-cwd signal"
        );
        assert_eq!(event_kind_count(&repo, "worktree.provisioned").await, 1);
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
        assert!(path.exists(), "plain lease release preserves the leaf");
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

    #[test]
    fn workspace_worktree_provision_recreates_stale_registered_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        init_git_repo(tmp.path());
        let target = WorkspaceLeaseTarget {
            repo_root: tmp.path().to_path_buf(),
            path: tmp.path().join(".claude/worktrees/wave-stale/card-stale"),
            branch: workspace_slice_branch_for("wave-stale", "card-stale").unwrap(),
        };

        provision_workspace_worktree(&target).unwrap();
        assert!(target.path.is_dir(), "initial worktree exists");
        std::fs::remove_dir_all(&target.path).unwrap();
        assert!(
            !target.path.exists(),
            "test setup leaves a registered but missing worktree path"
        );
        assert_ne!(
            git_worktree_registration(&target.repo_root, &target.path).unwrap(),
            GitWorktreeRegistration::Absent,
            "git still has a stale worktree registration"
        );

        provision_workspace_worktree(&target).unwrap();

        assert!(
            target.path.is_dir(),
            "stale registration is re-provisioned as a real worktree"
        );
        assert_eq!(
            git_worktree_registration(&target.repo_root, &target.path).unwrap(),
            GitWorktreeRegistration::Present
        );
        let top_level = git_stdout(&target.path, ["rev-parse", "--show-toplevel"]);
        assert_eq!(
            PathBuf::from(top_level.trim()).canonicalize().unwrap(),
            target.path.canonicalize().unwrap(),
            "re-provisioned path is a usable git worktree"
        );
    }

    #[test]
    fn workspace_worktree_provision_clears_stale_unregistered_non_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        init_git_repo(tmp.path());
        let target = WorkspaceLeaseTarget {
            repo_root: tmp.path().to_path_buf(),
            path: tmp
                .path()
                .join(".claude/worktrees/wave-unregistered/card-unregistered"),
            branch: workspace_slice_branch_for("wave-unregistered", "card-unregistered").unwrap(),
        };
        std::fs::create_dir_all(&target.path).unwrap();
        std::fs::write(target.path.join("stale.txt"), "partial worktree add\n").unwrap();
        assert_eq!(
            git_worktree_registration(&target.repo_root, &target.path).unwrap(),
            GitWorktreeRegistration::Absent,
            "test setup leaves a non-empty directory without worktree registration"
        );

        provision_workspace_worktree(&target).unwrap();

        assert!(
            target.path.is_dir(),
            "stale unregistered directory is re-provisioned as a real worktree"
        );
        assert!(
            !target.path.join("stale.txt").exists(),
            "stale unregistered contents are cleared before git worktree add"
        );
        assert_eq!(
            git_worktree_registration(&target.repo_root, &target.path).unwrap(),
            GitWorktreeRegistration::Present
        );
        let top_level = git_stdout(&target.path, ["rev-parse", "--show-toplevel"]);
        assert_eq!(
            PathBuf::from(top_level.trim()).canonicalize().unwrap(),
            target.path.canonicalize().unwrap(),
            "re-provisioned path is a usable git worktree"
        );
    }

    #[tokio::test]
    async fn workspace_worktree_provision_resolves_exclude_for_linked_wave_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("primary");
        init_git_repo(&primary);
        let linked = tmp.path().join("linked-wave");
        let linked_str = linked.to_str().unwrap();
        run_git(
            &primary,
            ["worktree", "add", "-b", "linked-wave", linked_str],
        );
        assert!(
            linked.join(".git").is_file(),
            "linked worktree .git is a gitdir file"
        );

        let (repo, wave_id, card_id) = lease_fixture(&linked).await;
        let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
        let target = prepare_workspace_lease_target_tx(&mut tx, &wave_id, &card_id)
            .await
            .unwrap();
        assert_eq!(
            target.repo_root.canonicalize().unwrap(),
            linked.canonicalize().unwrap()
        );
        let (_lease, _event) =
            acquire_workspace_lease_tx(&mut tx, &card_id, &wave_id, "op-test", &target)
                .await
                .unwrap();
        tx.commit().await.unwrap();

        provision_workspace_worktree(&target).unwrap();

        assert!(target.path.is_dir(), "provisioned worktree exists");
        assert_eq!(
            git_stdout(&linked, ["status", "--short", "--untracked-files=all"]),
            "",
            "linked wave worktree must stay clean after provisioning"
        );
        let exclude_path = git_exclude_path(&linked).unwrap();
        assert_eq!(
            exclude_path.canonicalize().unwrap(),
            primary.join(".git/info/exclude").canonicalize().unwrap()
        );
        let exclude = std::fs::read_to_string(&exclude_path).unwrap();
        assert_eq!(
            exclude
                .lines()
                .filter(|line| line.trim() == ".claude/worktrees/")
                .count(),
            1,
            "linked worktree exclude entry is written once"
        );

        provision_workspace_worktree(&target).unwrap();
        let exclude = std::fs::read_to_string(&exclude_path).unwrap();
        assert_eq!(
            exclude
                .lines()
                .filter(|line| line.trim() == ".claude/worktrees/")
                .count(),
            1,
            "linked worktree exclude entry remains idempotent"
        );
    }

    #[tokio::test]
    async fn card_release_preserves_worktree_branch_and_emits_no_removed_event() {
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
        std::fs::write(target.path.join("worker-output.txt"), "worker commit\n").unwrap();
        run_git(&target.path, ["add", "worker-output.txt"]);
        run_git(&target.path, ["commit", "-m", "worker output"]);

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
        assert!(
            target.path.is_dir(),
            "normal card release preserves the worker worktree"
        );
        assert!(
            git_ref_exists(&target.repo_root, &format!("refs/heads/{}", target.branch)).unwrap(),
            "normal card release preserves the slice branch"
        );
        assert_eq!(event_kind_count(&repo, "workspace.released").await, 1);
        assert_eq!(event_kind_count(&repo, "worktree.removed").await, 0);
    }

    #[tokio::test]
    async fn rollback_removes_worktree_before_releasing_lease_row() {
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

        let events = EventBus::new();
        assert!(
            remove_workspace_artifact_for_lease_by_id(repo.pool(), &events, &lease.lease_id)
                .await
                .unwrap()
        );
        assert!(
            release_workspace_lease_by_id(repo.pool(), &events, &lease.lease_id)
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
        assert!(
            !target.path.exists(),
            "rollback removal deletes the just-provisioned worktree"
        );
        assert!(
            !git_ref_exists(&target.repo_root, &format!("refs/heads/{}", target.branch)).unwrap(),
            "rollback removal deletes the just-created slice branch"
        );
        assert_eq!(event_kind_count(&repo, "workspace.released").await, 1);
        assert_eq!(event_kind_count(&repo, "worktree.removed").await, 1);
    }

    #[tokio::test]
    async fn release_by_id_removes_artifact_before_workspace_released() {
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

        let events = EventBus::new();
        assert!(
            release_workspace_lease_by_id(repo.pool(), &events, &lease.lease_id)
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
        assert!(
            !target.path.exists(),
            "by-id compensating release removes the worktree artifact"
        );
        assert!(
            !git_ref_exists(&target.repo_root, &format!("refs/heads/{}", target.branch)).unwrap(),
            "by-id compensating release removes the slice branch"
        );
        let kinds: Vec<String> = sqlx::query_scalar(
            "SELECT kind FROM events \
             WHERE kind IN ('worktree.removed', 'workspace.released') \
             ORDER BY id ASC",
        )
        .fetch_all(repo.pool())
        .await
        .unwrap();
        assert_eq!(kinds, vec!["worktree.removed", "workspace.released"]);
    }

    #[tokio::test]
    async fn wave_release_sweeps_worktrees_plain_dirs_and_branches_post_commit() {
        let tmp = tempfile::tempdir().unwrap();
        init_git_repo(tmp.path());
        let (repo, wave_id, card_id) = lease_fixture(tmp.path()).await;

        let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
        let target = prepare_workspace_lease_target_tx(&mut tx, &wave_id, &card_id)
            .await
            .unwrap();
        let (_lease, _event) =
            acquire_workspace_lease_tx(&mut tx, &card_id, &wave_id, "op-test", &target)
                .await
                .unwrap();
        tx.commit().await.unwrap();
        provision_workspace_worktree(&target).unwrap();

        let events = EventBus::new();
        release_workspace_lease_for_card_repo(&repo, &events, &card_id)
            .await
            .unwrap();
        assert!(
            target.path.is_dir(),
            "preserved worktree exists after normal release"
        );
        assert!(
            git_ref_exists(&target.repo_root, &format!("refs/heads/{}", target.branch)).unwrap(),
            "preserved branch exists after normal release"
        );
        let plain_card_id = "plain-card";
        let plain_path = tmp
            .path()
            .join(".claude")
            .join("worktrees")
            .join(&wave_id)
            .join(plain_card_id);
        std::fs::create_dir_all(&plain_path).unwrap();
        std::fs::write(plain_path.join("leftover.txt"), "plain leftover\n").unwrap();
        let plain_branch = workspace_slice_branch_for(&wave_id, plain_card_id).unwrap();
        run_git(tmp.path(), ["branch", &plain_branch]);
        let branch_only = workspace_slice_branch_for(&wave_id, "branch-only").unwrap();
        run_git(tmp.path(), ["branch", &branch_only]);

        let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
        let release = release_workspace_leases_for_wave_tx(&mut tx, &wave_id)
            .await
            .unwrap();
        tx.commit().await.unwrap();

        assert!(
            release.events.is_empty(),
            "released lease rows do not emit another workspace release"
        );
        let sweep = release.sweep.expect("wave sweep plan");
        assert_eq!(
            sweep_workspace_worktrees_for_wave_repo(&repo, &events, sweep.clone())
                .await
                .unwrap(),
            2
        );
        assert!(
            !target.path.exists(),
            "wave teardown sweeps preserved worktree paths"
        );
        assert!(
            !plain_path.exists(),
            "wave teardown sweeps leftover plain workspace dirs"
        );
        assert!(
            !git_ref_exists(&target.repo_root, &format!("refs/heads/{}", target.branch)).unwrap(),
            "wave teardown sweeps preserved slice branches"
        );
        assert!(
            !git_ref_exists(&target.repo_root, &format!("refs/heads/{plain_branch}")).unwrap(),
            "wave teardown sweeps plain-dir slice branches"
        );
        assert!(
            !git_ref_exists(&target.repo_root, &format!("refs/heads/{branch_only}")).unwrap(),
            "wave teardown sweeps branch-only slice branches"
        );
        assert_eq!(event_kind_count(&repo, "worktree.removed").await, 2);
        assert_eq!(
            sweep_workspace_worktrees_for_wave_repo(&repo, &events, sweep)
                .await
                .unwrap(),
            0,
            "wave sweep is idempotent after paths are gone"
        );
        assert_eq!(
            event_kind_count(&repo, "worktree.removed").await,
            2,
            "idempotent sweep emits no duplicate removal events"
        );
    }

    #[tokio::test]
    async fn wave_sweep_uses_persisted_lease_paths_when_wave_cwd_is_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        init_git_repo(tmp.path());
        let wave_cwd = tmp.path().join("deleted-wave-cwd");
        std::fs::create_dir_all(&wave_cwd).unwrap();
        let (repo, wave_id, card_id) = lease_fixture(&wave_cwd).await;

        let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
        let target = prepare_workspace_lease_target_tx(&mut tx, &wave_id, &card_id)
            .await
            .unwrap();
        let (_lease, _event) =
            acquire_workspace_lease_tx(&mut tx, &card_id, &wave_id, "op-test", &target)
                .await
                .unwrap();
        tx.commit().await.unwrap();
        provision_workspace_worktree(&target).unwrap();
        assert!(target.path.is_dir(), "test setup provisioned worktree");

        let events = EventBus::new();
        release_workspace_lease_for_card_repo(&repo, &events, &card_id)
            .await
            .unwrap();
        std::fs::remove_dir_all(&wave_cwd).unwrap();
        assert!(
            git_repo_root_for_wave_cwd(&wave_id, wave_cwd.to_str().unwrap()).is_err(),
            "test setup leaves wave.cwd unusable for git -C"
        );

        let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
        let release = release_workspace_leases_for_wave_tx(&mut tx, &wave_id)
            .await
            .unwrap();
        tx.commit().await.unwrap();

        assert!(
            release.events.is_empty(),
            "released lease rows do not emit another workspace release"
        );
        let sweep = release.sweep.expect("wave sweep plan");
        assert_eq!(
            sweep_workspace_worktrees_for_wave_repo(&repo, &events, sweep)
                .await
                .unwrap(),
            1
        );
        assert!(
            !target.path.exists(),
            "sweep removes worktree using repo root recovered from persisted lease path"
        );
        assert!(
            !git_ref_exists(&target.repo_root, &format!("refs/heads/{}", target.branch)).unwrap(),
            "sweep removes branch using repo root recovered from persisted lease path"
        );
        assert_eq!(event_kind_count(&repo, "worktree.removed").await, 1);
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

    async fn event_kind_count(repo: &crate::db::sqlite::SqlxRepo, kind: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = ?1")
            .bind(kind)
            .fetch_one(repo.pool())
            .await
            .unwrap()
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
