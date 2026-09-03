use std::{
    collections::BTreeSet,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Output},
};

use sqlx::{QueryBuilder, Row, Sqlite, SqlitePool};

use crate::db::sqlite::{append_decision_event_in_tx, begin_immediate_tx};
use crate::db::{RepoEventWrite, write_in_tx_typed};
use crate::error::{CalmError, Result};
use crate::event::{BroadcastEnvelope, Event, EventBus, EventScope, SYNC_EVENT_VERSION};
use crate::ids::{ActorId, AreaId, CardId, TrackId};
use crate::model::{TrackWorkspaceKind, new_id, now_ms};
use crate::proc_identity::read_boot_id;

use super::forge_action_adapter::FORGE_ACTION_KIND;
use super::{PhaseTag, TimestampMs, Tx};

#[derive(Clone, Debug)]
pub(crate) struct WorkspaceLease {
    pub lease_id: String,
    pub card_id: String,
    pub track_id: String,
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
///
/// This is a non-transactional read used before the teardown transaction, so it
/// has a TOCTOU window: a forge-action could enter a recoverable phase after
/// this check and before the worktree sweep. It shrinks the route-level race;
/// the durable forge-op parked-recovery contract remains the real backstop.
/// The airtight in-tx/lease-hold guard is intentionally left to slice ⑤.
pub(crate) async fn track_has_active_forge_action(
    pool: &SqlitePool,
    track_id: &str,
) -> Result<bool> {
    any_track_has_active_forge_action(pool, &[track_id]).await
}

/// Area-friendly variant of [`track_has_active_forge_action`].
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
    track_id: &str,
    card_id: &str,
    workspace_root: &Path,
) -> Result<WorkspaceLeaseTarget> {
    validate_path_segment("track_id", track_id)?;
    validate_path_segment("card_id", card_id)?;
    // #1147 S1 — `tracks.cwd` dropped by migration 0077.
    let (kind, cwd): (String, String) =
        sqlx::query_as("SELECT workspace_kind, workspace_path FROM tracks WHERE id = ?1")
            .bind(track_id)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("track {track_id}")))?;
    // #1147 S2 (red-team B5) — last-chance materialize before a worker commits
    // to this directory.
    //
    // Track create materializes too, but that call happens after its
    // transaction commits, so a failure there leaves a committed track row
    // pointing at a directory that does not exist, and NO other path would
    // ever retry it. Every codex task on such a track would then die in
    // `git rev-parse --show-toplevel` with nothing but `spawn-failed`
    // visible — which is precisely the bug #1147 was opened on, re-created by
    // the slice meant to fix it.
    //
    // Idempotent and ~one `rev-parse` in the steady state (see
    // `materialize_managed_workspace`), and a no-op for attached workspaces —
    // those are the user's directories and must never be created or
    // `git init`-ed here.
    if TrackWorkspaceKind::try_from(kind).map_err(CalmError::Internal)?
        == TrackWorkspaceKind::Managed
    {
        crate::workspace_materialize::materialize_managed_workspace(
            workspace_root,
            Path::new(&cwd),
            track_id,
        )?;
    }
    let repo_root = git_repo_root_for_track_cwd(track_id, &cwd)?;
    Ok(WorkspaceLeaseTarget {
        path: workspace_lease_path_for(&repo_root, track_id, card_id)?,
        branch: workspace_slice_branch_for(track_id, card_id)?,
        repo_root,
    })
}

pub(crate) async fn acquire_workspace_lease_tx(
    tx: &mut Tx<'_>,
    card_id: &str,
    track_id: &str,
    lease_owner: &str,
    target: &WorkspaceLeaseTarget,
) -> Result<(WorkspaceLease, BroadcastEnvelope)> {
    acquire_workspace_lease_at_path_tx(
        tx,
        card_id,
        track_id,
        lease_owner,
        &target.path,
        WorkspaceLeaseDirectoryMode::ParentOnly,
    )
    .await
}

pub(crate) async fn acquire_plain_workspace_lease_tx(
    tx: &mut Tx<'_>,
    card_id: &str,
    track_id: &str,
    lease_owner: &str,
    path: &Path,
) -> Result<(WorkspaceLease, BroadcastEnvelope)> {
    acquire_workspace_lease_at_path_tx(
        tx,
        card_id,
        track_id,
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
    track_id: &str,
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
               lease_id, card_id, track_id, path, state, lease_owner,
               lease_until_ms, boot_id, created_at_ms, updated_at_ms
           )
           VALUES (?1, ?2, ?3, ?4, 'held', ?5, ?6, ?7, ?8, ?8)"#,
    )
    .bind(&lease_id)
    .bind(card_id)
    .bind(track_id)
    .bind(&path_string)
    .bind(lease_owner)
    .bind(now + WORKSPACE_LEASE_MS)
    .bind(&boot_id)
    .bind(now)
    .execute(&mut **tx)
    .await?;

    // #1147 S3 — freeze point 1 of 4 (design §更换与冻结): "the first workspace
    // lease". A lease row stores an absolute path derived from the track's
    // workspace, and the worktree it is about to create is anchored to that
    // repository by two absolute pointers (`<wt>/.git` and
    // `<repo>/.git/worktrees/<n>/gitdir`) that a rename would leave dangling
    // in both directions. Nothing re-anchors either, so the workspace has to
    // stop moving before this row exists.
    //
    // Here rather than in the two `acquire_*` wrappers: this is the single
    // statement both of them bottom out in, so a third lease flavour added
    // later inherits the freeze instead of having to remember it. The system
    // area is excluded inside the freeze itself — the launchpad takes leases
    // on every codex task and is the one track whose path the kernel keeps
    // re-deriving.
    crate::db::sqlite::track_workspace_freeze_tx(tx, track_id, now).await?;

    create_workspace_lease_directory(path, directory_mode)?;

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
                r#"SELECT lease_id, card_id, track_id, path, state, boot_id
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
        r#"SELECT lease_id, card_id, track_id, path, state, boot_id
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
    let scope = workspace_scope_tx(&mut tx, &lease.card_id, &lease.track_id).await?;
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
        track_id: TrackId::from(lease.track_id.clone()),
        card_id: CardId::from(lease.card_id.clone()),
        lease_id: lease.lease_id.clone(),
    };
    let event_id =
        append_decision_event_in_tx(&mut tx, &ActorId::KernelDispatcher, &scope, None, &event)
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

pub(crate) async fn release_workspace_leases_for_track_tx(
    tx: &mut Tx<'_>,
    track_id: &str,
) -> Result<WorkspaceTrackRelease> {
    let sweep = workspace_track_sweep_for_track_tx(tx, track_id).await;
    let rows = sqlx::query(
        r#"SELECT lease_id, card_id, track_id, path, state, boot_id
           FROM workspace_leases
           WHERE track_id = ?1
             AND state IN ('held','releasing')
           ORDER BY created_at_ms ASC, lease_id ASC"#,
    )
    .bind(track_id)
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
    Ok(WorkspaceTrackRelease { events, sweep })
}

async fn release_workspace_lease_tx(
    tx: &mut Tx<'_>,
    lease: WorkspaceLease,
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
            track_id: TrackId::from(lease.track_id),
            card_id: CardId::from(lease.card_id),
            lease_id: lease.lease_id,
        },
    )])
}

#[derive(Clone, Debug)]
pub(crate) struct WorkspaceTrackRelease {
    pub(crate) events: Vec<(ActorId, EventScope, Event)>,
    pub(crate) sweep: Option<WorkspaceTrackSweep>,
}

#[derive(Clone, Debug)]
pub(crate) struct WorkspaceTrackSweep {
    track_id: String,
    area_id: String,
    cwd: String,
    leases: Vec<WorkspaceLease>,
}

async fn workspace_track_sweep_for_track_tx(
    tx: &mut Tx<'_>,
    track_id: &str,
) -> Option<WorkspaceTrackSweep> {
    if let Err(error) = validate_path_segment("track_id", track_id) {
        tracing::warn!(
            track_id,
            error = %error,
            "workspace track teardown skipped preserved worktree sweep for invalid track id"
        );
        return None;
    }
    // #1147 S1 — `tracks.cwd` dropped by migration 0077.
    let row = match sqlx::query("SELECT workspace_path, area_id FROM tracks WHERE id = ?1")
        .bind(track_id)
        .fetch_optional(&mut **tx)
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => return None,
        Err(error) => {
            tracing::warn!(
                track_id,
                error = %error,
                "workspace track teardown could not read cwd for preserved worktree sweep"
            );
            return None;
        }
    };
    // #1147 S1 — by NAME, so it had to move with the SELECT above. `try_get`
    // resolves at runtime; a stale name here degrades the sweep to a silent
    // `None` + warn, which is why it took a test to catch rather than rustc.
    let cwd: String = match row.try_get("workspace_path") {
        Ok(cwd) => cwd,
        Err(error) => {
            tracing::warn!(
                track_id,
                error = %error,
                "workspace track teardown could not read workspace_path for preserved worktree sweep"
            );
            return None;
        }
    };
    let area_id: String = match row.try_get("area_id") {
        Ok(area_id) => area_id,
        Err(error) => {
            tracing::warn!(
                track_id,
                error = %error,
                "workspace track teardown could not read area_id column for preserved worktree sweep"
            );
            return None;
        }
    };
    let leases = match sqlx::query(
        r#"SELECT lease_id, card_id, track_id, path, state, boot_id
           FROM workspace_leases
           WHERE track_id = ?1
           ORDER BY created_at_ms ASC, lease_id ASC"#,
    )
    .bind(track_id)
    .fetch_all(&mut **tx)
    .await
    {
        Ok(rows) => rows
            .into_iter()
            .filter_map(|row| match row_to_workspace_lease(row) {
                Ok(lease) => Some(lease),
                Err(error) => {
                    tracing::warn!(
                        track_id,
                        error = %error,
                        "workspace track teardown skipped unparseable persisted lease row"
                    );
                    None
                }
            })
            .collect(),
        Err(error) => {
            tracing::warn!(
                track_id,
                error = %error,
                "workspace track teardown could not read persisted lease paths for sweep"
            );
            Vec::new()
        }
    };
    Some(WorkspaceTrackSweep {
        track_id: track_id.to_string(),
        area_id,
        cwd,
        leases,
    })
}

#[derive(Clone, Debug)]
struct RemovedWorkspaceWorktree {
    card_id: String,
    path: String,
}

pub(crate) async fn sweep_workspace_worktrees_for_track_repo(
    repo: &dyn RepoEventWrite,
    events: &EventBus,
    sweep: WorkspaceTrackSweep,
) -> Result<usize> {
    let repo_roots = repo_roots_for_track_sweep(&sweep);
    if repo_roots.is_empty() {
        return Ok(0);
    }
    let mut removed = Vec::new();
    for repo_root in repo_roots {
        removed.extend(sweep_workspace_worktree_root_for_track(
            &repo_root,
            &sweep.track_id,
        ));
        sweep_workspace_slice_branches_for_track(&repo_root, &sweep.track_id);
    }
    let removed_count = removed.len();
    if removed.is_empty() {
        return Ok(0);
    }
    let envelopes = persist_track_sweep_removed_events(repo, &sweep, removed).await?;
    for envelope in envelopes {
        events.emit_envelope(envelope);
    }
    Ok(removed_count)
}

fn repo_roots_for_track_sweep(sweep: &WorkspaceTrackSweep) -> Vec<PathBuf> {
    let mut roots = BTreeSet::new();
    for lease in &sweep.leases {
        match workspace_lease_target_from_lease(lease) {
            Ok(Some(target)) => {
                roots.insert(target.repo_root);
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    track_id = %sweep.track_id,
                    lease_id = %lease.lease_id,
                    path = %lease.path,
                    error = %error,
                    "workspace track teardown skipped invalid persisted lease path"
                );
            }
        }
    }
    if !roots.is_empty() {
        return roots.into_iter().collect();
    }
    match git_repo_root_for_track_cwd(&sweep.track_id, &sweep.cwd) {
        Ok(repo_root) => vec![repo_root],
        Err(error) => {
            tracing::error!(
                track_id = %sweep.track_id,
                cwd = %sweep.cwd,
                error = %error,
                "workspace track teardown could not derive repo root from persisted lease paths or track cwd"
            );
            Vec::new()
        }
    }
}

pub(crate) async fn sweep_workspace_worktrees_for_tracks_repo(
    repo: &dyn RepoEventWrite,
    events: &EventBus,
    sweeps: Vec<WorkspaceTrackSweep>,
) -> Result<usize> {
    let mut removed = 0;
    for sweep in sweeps {
        removed += sweep_workspace_worktrees_for_track_repo(repo, events, sweep).await?;
    }
    Ok(removed)
}

fn sweep_workspace_worktree_root_for_track(
    repo_root: &Path,
    track_id: &str,
) -> Vec<RemovedWorkspaceWorktree> {
    let mut removed = Vec::new();
    let track_root = repo_root.join(".claude").join("worktrees").join(track_id);
    let entries = match std::fs::read_dir(&track_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return removed,
        Err(error) => {
            tracing::warn!(
                repo_root = %repo_root.display(),
                track_id,
                path = %track_root.display(),
                error = %error,
                "workspace track teardown could not read preserved worktree root"
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
                    track_id,
                    error = %error,
                    "workspace track teardown could not read preserved worktree entry"
                );
                continue;
            }
        };
        let path = entry.path();
        let Some(parts) = workspace_lease_path_parts(&path) else {
            tracing::warn!(
                repo_root = %repo_root.display(),
                track_id,
                path = %path.display(),
                "workspace track teardown skipped non-lease-shaped preserved worktree path"
            );
            continue;
        };
        if parts.repo_root.as_path() != repo_root || parts.track_id != track_id {
            tracing::warn!(
                repo_root = %repo_root.display(),
                track_id,
                path = %path.display(),
                "workspace track teardown skipped preserved worktree outside track root"
            );
            continue;
        }
        if let Err(error) = validate_path_segment("card_id", &parts.card_id) {
            tracing::warn!(
                repo_root = %repo_root.display(),
                track_id,
                card_id = %parts.card_id,
                path = %path.display(),
                error = %error,
                "workspace track teardown skipped preserved worktree with invalid card id"
            );
            continue;
        }
        let target = WorkspaceLeaseTarget {
            repo_root: repo_root.to_path_buf(),
            path,
            branch: match workspace_slice_branch_for(track_id, &parts.card_id) {
                Ok(branch) => branch,
                Err(error) => {
                    tracing::warn!(
                        repo_root = %repo_root.display(),
                        track_id,
                        card_id = %parts.card_id,
                        error = %error,
                        "workspace track teardown skipped preserved worktree branch derivation"
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
                    track_id,
                    card_id = %parts.card_id,
                    path = %target.path.display(),
                    error = %error,
                    "workspace track teardown could not remove preserved worktree"
                );
            }
        }
    }
    match std::fs::remove_dir(&track_root) {
        Ok(()) => {}
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
            ) => {}
        Err(error) => {
            tracing::warn!(
                repo_root = %repo_root.display(),
                track_id,
                path = %track_root.display(),
                error = %error,
                "workspace track teardown could not remove empty preserved worktree root"
            );
        }
    }
    removed
}

fn sweep_workspace_slice_branches_for_track(repo_root: &Path, track_id: &str) {
    let branch_prefix = format!("neige/{track_id}/");
    let ref_prefix = format!("refs/heads/neige/{track_id}");
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
                track_id,
                error = %error,
                "workspace track teardown could not list preserved slice branches"
            );
            return;
        }
    };
    if !output.status.success() {
        tracing::warn!(
            repo_root = %repo_root.display(),
            track_id,
            error = %git_failed("git for-each-ref", repo_root, &output),
            "workspace track teardown could not list preserved slice branches"
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
                    track_id,
                    branch,
                    error = %error,
                    "workspace track teardown could not spawn preserved branch delete"
                );
                continue;
            }
        };
        match git_ref_exists(repo_root, &branch_ref) {
            Ok(true) if !output.status.success() => {
                tracing::warn!(
                    repo_root = %repo_root.display(),
                    track_id,
                    branch,
                    error = %git_failed("git branch -D", repo_root, &output),
                    "workspace track teardown could not delete preserved slice branch"
                );
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(
                    repo_root = %repo_root.display(),
                    track_id,
                    branch,
                    error = %error,
                    "workspace track teardown could not verify preserved slice branch deletion"
                );
            }
        }
    }
}

async fn persist_track_sweep_removed_events(
    repo: &dyn RepoEventWrite,
    sweep: &WorkspaceTrackSweep,
    removed: Vec<RemovedWorkspaceWorktree>,
) -> Result<Vec<BroadcastEnvelope>> {
    let track_id = sweep.track_id.clone();
    let area_id = sweep.area_id.clone();
    write_in_tx_typed(repo, move |tx| {
        let track_id = track_id.clone();
        let area_id = area_id.clone();
        let removed = removed.clone();
        Box::pin(async move {
            let mut events = Vec::with_capacity(removed.len());
            for removed in removed {
                let scope = EventScope::Card {
                    card: CardId::from(removed.card_id.clone()),
                    track: TrackId::from(track_id.clone()),
                    area: AreaId::from(area_id.clone()),
                };
                events.push((
                    ActorId::KernelDispatcher,
                    scope,
                    Event::WorktreeRemoved {
                        track_id: TrackId::from(track_id.clone()),
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
    let scope = workspace_scope_tx(&mut tx, &lease.card_id, &lease.track_id).await?;
    let event = Event::WorktreeRemoved {
        track_id: TrackId::from(lease.track_id.clone()),
        card_id: CardId::from(lease.card_id.clone()),
        path: lease.path.clone(),
    };
    let event_id =
        append_decision_event_in_tx(&mut tx, &ActorId::KernelDispatcher, &scope, None, &event)
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

async fn workspace_scope_tx(tx: &mut Tx<'_>, card_id: &str, track_id: &str) -> Result<EventScope> {
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

async fn workspace_lease_by_id(
    pool: &SqlitePool,
    lease_id: &str,
) -> Result<Option<WorkspaceLease>> {
    let row = sqlx::query(
        r#"SELECT lease_id, card_id, track_id, path, state, boot_id
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
        r#"SELECT lease_id, card_id, track_id, path, state, boot_id
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
        track_id: row.try_get("track_id")?,
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
    RECOVERABLE_OPERATION_PHASES
        .iter()
        .any(|tag| tag.as_str() == phase)
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

pub(crate) fn ensure_workspace_worktree_root_excluded(repo_root: &Path) -> Result<()> {
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
    // #1147 S2 (red-team B6) — env-isolated: this runs as part of
    // materialization, and an inherited `GIT_DIR` would send the exclude file
    // into a completely different repository.
    let output = crate::workspace_materialize::neige_git_command()
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
    track_id: &str,
    card_id: &str,
) -> Result<PathBuf> {
    validate_path_segment("track_id", track_id)?;
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
        .join(track_id)
        .join(card_id))
}

pub(crate) fn plain_workspace_lease_path_for(track_id: &str, card_id: &str) -> Result<PathBuf> {
    validate_path_segment("track_id", track_id)?;
    validate_path_segment("card_id", card_id)?;
    Ok(PathBuf::from(".claude")
        .join("worktrees")
        .join(track_id)
        .join(card_id))
}

pub(crate) fn workspace_slice_branch_for(track_id: &str, card_id: &str) -> Result<String> {
    validate_path_segment("track_id", track_id)?;
    validate_path_segment("card_id", card_id)?;
    Ok(format!("neige/{track_id}/{card_id}"))
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

pub(crate) fn git_repo_root_for_track_cwd(track_id: &str, cwd: &str) -> Result<PathBuf> {
    let cwd_path = Path::new(cwd);
    if cwd.trim().is_empty() || !cwd_path.is_absolute() {
        return Err(CalmError::BadRequest(format!(
            "track {track_id} cwd must be an absolute git repository path for workspace leasing"
        )));
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd_path)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| {
            CalmError::Internal(format!(
                "spawn git rev-parse --show-toplevel for track {track_id} cwd {}: {e}",
                cwd_path.display()
            ))
        })?;
    if !output.status.success() {
        return Err(CalmError::BadRequest(format!(
            "track {track_id} cwd {} is not a git repository: {}",
            cwd_path.display(),
            output_summary(&output)
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let repo_root = stdout.trim_end_matches(&['\r', '\n'][..]);
    if repo_root.is_empty() {
        return Err(CalmError::BadRequest(format!(
            "track {track_id} cwd {} did not resolve to a git repository root",
            cwd_path.display()
        )));
    }
    let repo_root = PathBuf::from(repo_root);
    if !repo_root.is_absolute() {
        return Err(CalmError::BadRequest(format!(
            "track {track_id} git repository root must be absolute: {}",
            repo_root.display()
        )));
    }
    Ok(repo_root)
}

fn workspace_lease_target_from_lease(
    lease: &WorkspaceLease,
) -> Result<Option<WorkspaceLeaseTarget>> {
    validate_path_segment("track_id", &lease.track_id)?;
    validate_path_segment("card_id", &lease.card_id)?;
    let path = PathBuf::from(&lease.path);
    let Some(parts) = workspace_lease_path_parts(&path) else {
        return Ok(None);
    };
    if parts.card_id != lease.card_id || parts.track_id != lease.track_id {
        return Ok(None);
    }
    Ok(Some(WorkspaceLeaseTarget {
        repo_root: parts.repo_root,
        path,
        branch: workspace_slice_branch_for(&lease.track_id, &lease.card_id)?,
    }))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkspaceLeasePathParts {
    repo_root: PathBuf,
    track_id: String,
    card_id: String,
}

fn workspace_lease_path_parts(path: &Path) -> Option<WorkspaceLeasePathParts> {
    if !path.is_absolute() {
        return None;
    }
    let card_id = path.file_name()?.to_str()?;
    let track_path = path.parent()?;
    let track_id = track_path.file_name()?.to_str()?;
    let worktrees_path = track_path.parent()?;
    let worktrees_dir = worktrees_path.file_name()?.to_str()?;
    let claude_path = worktrees_path.parent()?;
    let claude_dir = claude_path.file_name()?.to_str()?;
    let repo_root = claude_path.parent()?;
    if worktrees_dir != "worktrees" || claude_dir != ".claude" || !repo_root.is_absolute() {
        return None;
    }
    Some(WorkspaceLeasePathParts {
        repo_root: repo_root.to_path_buf(),
        track_id: track_id.to_string(),
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
    validate_path_segment("track_id", &parts.track_id)?;
    validate_path_segment("card_id", &parts.card_id)?;
    if parts.repo_root.as_path() != target.repo_root.as_path() {
        return Err(CalmError::Internal(format!(
            "refusing to clear workspace worktree path {} outside repo root {}",
            target.path.display(),
            target.repo_root.display()
        )));
    }
    let expected_branch = workspace_slice_branch_for(&parts.track_id, &parts.card_id)?;
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
mod tests;
