use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    process::Output,
};

use sqlx::{QueryBuilder, Row, Sqlite, SqlitePool};

use crate::db::sqlite::append_decision_event_in_tx;
use crate::error::{CalmError, Result};
use crate::event::{BroadcastEnvelope, Event, EventScope, SYNC_EVENT_VERSION};
use crate::ids::{ActorId, AreaId, CardId, TrackId};
use crate::model::{TaskAccess, new_id, now_ms};
use crate::proc_identity::read_boot_id;
use crate::workspace_materialize::{isolated_git_command, neige_git_command};

use super::forge_action_adapter::FORGE_ACTION_KIND;
use super::{PhaseTag, TimestampMs, Tx};

pub(crate) mod base;
pub(crate) mod facts;
pub(crate) mod release;
pub(crate) mod track_worktree;
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
pub(crate) use worker::{WorkerLeasePlan, prepare_worker_lease_tx, worker_branch_tx};

/// The one SELECT list every reader of a lease row uses
/// (`row_to_workspace_lease` takes columns by name at run time, so a column
/// missing from any one SELECT is a `ColumnNotFound` no compiler sees). The
/// calm-truth read `db/sqlite/read.rs` `workspace_lease_for_card` builds its
/// own five-field struct and is deliberately not on this list.
pub(crate) const WORKSPACE_LEASE_COLUMNS: &str = "lease_id, card_id, track_id, path, state, boot_id, \
     base_sha, base_source, base_attempt_id, canonical_path, git_common_dir, delivery_policy";

#[derive(Clone, Debug)]
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

/// A kernel-made worktree as a git target: the track worktree (#1830 S1) that
/// [`remove_workspace_worktree`] and `track_worktree::ensure_track_worktree` act on.
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

impl WorkspaceLeaseTarget {
    pub(crate) fn path_string(&self) -> String {
        self.path.to_string_lossy().to_string()
    }
}

/// INSERT the lease row of one worker attempt at the directory [`prepare_worker_lease_tx`] chose
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
    let base = match plan.access {
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
        plan.access,
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
    let sweep = workspace_track_sweep_for_track_tx(tx, track_id).await?;
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

/// What a track teardown removes after its commit (D10): the track worktree and its branch
/// (#1830 S1), and the candidate refs in every common dir a lease of the track recorded.
#[derive(Clone, Debug)]
pub(crate) struct WorkspaceTrackSweep {
    track_id: String,
    /// `tracks.workspace_worktree_path`: removed with the track, whatever its leases.
    track_worktree: Option<String>,
    git_common_dirs: Vec<PathBuf>,
}

async fn workspace_track_sweep_for_track_tx(
    tx: &mut Tx<'_>,
    track_id: &str,
) -> Result<Option<WorkspaceTrackSweep>> {
    let Some(track_worktree) = sqlx::query_scalar::<_, Option<String>>(
        "SELECT workspace_worktree_path FROM tracks WHERE id = ?1",
    )
    .bind(track_id)
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Ok(None);
    };
    let git_common_dirs = sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT git_common_dir FROM workspace_leases \
         WHERE track_id = ?1 AND git_common_dir IS NOT NULL ORDER BY git_common_dir",
    )
    .bind(track_id)
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(PathBuf::from)
    .collect();
    Ok(Some(WorkspaceTrackSweep {
        track_id: track_id.to_string(),
        track_worktree,
        git_common_dirs,
    }))
}

/// The post-commit half of a track or area teardown, best effort: the track worktree, then the
/// candidate refs, addressed by the lease rows' common dirs (not by the track cwd, which may be a
/// moved linked worktree). Never fails the caller: the track rows are already gone.
pub(crate) fn sweep_workspace_worktrees_for_tracks(sweeps: Vec<WorkspaceTrackSweep>) {
    for sweep in sweeps {
        if let Some(track_worktree) = &sweep.track_worktree {
            track_worktree::remove_track_worktree(&sweep.track_id, track_worktree);
        }
        crate::git_candidate::refs::delete_candidate_refs_for_track(
            &sweep.track_id,
            sweep.git_common_dirs.iter().map(PathBuf::as_path),
        );
    }
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

fn remove_workspace_dir_if_exists(path: &str) -> Result<bool> {
    let path = Path::new(path);
    // A symlink leaf is unlinked, never followed.
    if base::unlink_symlink_leaf(path)? {
        return Ok(true);
    }
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

/// Discard a kernel-made worktree and its branch, whatever the checkout holds: a symlink leaf is
/// unlinked (never followed) and what git registered there pruned; someone else's registration
/// at the path's realpath is refused; else `worktree remove --force`, `branch -D`, and a plain
/// directory removal. A repository that is gone leaves only the directory to remove. `true` when
/// anything was removed. The track worktree teardown (#1830 S1) is the one caller.
pub(crate) fn remove_workspace_worktree(target: &WorkspaceLeaseTarget) -> Result<bool> {
    if !git_repo_available(&target.repo_root) {
        return remove_workspace_dir_if_exists(&target.path_string());
    }

    // A symlink leaf is never a registration of ours: unlink it and prune
    // what git registered at the now-missing path (a worktree moved away and
    // linked back would otherwise keep its branch checked out and fail the
    // `branch -D` below). `worktree remove --force` through the link would
    // delete the link's target — an external directory, the main checkout.
    let link_removed = base::unlink_symlink_leaf(&target.path)?;
    if link_removed {
        git_worktree_prune(&target.repo_root)?;
    }
    let registration = if link_removed {
        GitWorktreeRegistration::Absent
    } else {
        git_worktree_registration(target)?
    };
    // Someone else's worktree at our realpath: `worktree remove --force`
    // would delete it through the alias.
    if let GitWorktreeRegistration::Foreign {
        registered_as,
        branch,
    } = registration
    {
        return Err(base::foreign_registration_refusal(
            target,
            &registered_as,
            branch.as_deref(),
        ));
    }
    let registered = registration != GitWorktreeRegistration::Absent;
    let path_existed = !link_removed && target.path.exists();
    if registered || path_existed {
        let output = neige_git_command()
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
        if !output.status.success() && registered && git_worktree_registered(target)? {
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
        // Isolated: a ref deletion runs the repository's `reference-transaction` hook.
        let output = isolated_git_command()
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
    Ok(link_removed || registered || path_existed || branch_existed || dir_removed)
}

const WORKTREE_EXCLUDE: &str = ".claude/worktrees/";

pub(crate) fn ensure_workspace_worktree_root_excluded(repo_root: &Path) -> Result<()> {
    ensure_git_exclude_entry(repo_root, WORKTREE_EXCLUDE)
}

/// Append `entry` to `<git-dir>/info/exclude` unless a line already equals it.
/// The match is exact on purpose: a near-miss (`.neige` vs `.neige/`) would still hide the directory but append a second line on every call.
pub(crate) fn ensure_git_exclude_entry(repo_root: &Path, entry: &str) -> Result<()> {
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
    if existing.lines().any(|line| line.trim() == entry) {
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
    file.write_all(format!("{entry}\n").as_bytes())
        .map_err(|error| {
            CalmError::Internal(format!(
                "write git exclude {}: {error}",
                exclude_path.display()
            ))
        })?;
    Ok(())
}

fn git_exclude_path(repo_root: &Path) -> Result<PathBuf> {
    // Env-isolated: an inherited `GIT_DIR` would send the exclude file into a different repository.
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
    let output = neige_git_command()
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
    let repo_root = base::printed_path(&output.stdout);
    if repo_root.as_os_str().is_empty() {
        return Err(CalmError::BadRequest(format!(
            "track {track_id} cwd {} did not resolve to a git repository root",
            cwd_path.display()
        )));
    }
    if !repo_root.is_absolute() {
        return Err(CalmError::BadRequest(format!(
            "track {track_id} git repository root must be absolute: {}",
            repo_root.display()
        )));
    }
    // Refused where it is read: `repo_root` is stored and frozen
    // through `json!` by both adapters, so it is UTF-8 by construction.
    base::utf8_path(&repo_root, "track git repository root")?;
    Ok(repo_root)
}

fn git_repo_available(repo_root: &Path) -> bool {
    neige_git_command()
        .arg("-C")
        .arg(repo_root)
        .args(["rev-parse", "--git-dir"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn git_ref_exists(repo_root: &Path, full_ref: &str) -> Result<bool> {
    let status = neige_git_command()
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum GitWorktreeRegistration {
    Absent,
    Present,
    Prunable,
    /// A registration at the lease path's realpath recorded under another
    /// path (`registered_as`; `branch` is its `refs/heads/..`, `None` when
    /// detached): an alias of someone else's worktree, never reused, pruned
    /// or removed.
    Foreign {
        registered_as: PathBuf,
        branch: Option<String>,
    },
}

fn git_worktree_registered(target: &WorkspaceLeaseTarget) -> Result<bool> {
    Ok(git_worktree_registration(target)? != GitWorktreeRegistration::Absent)
}

fn git_worktree_registration(target: &WorkspaceLeaseTarget) -> Result<GitWorktreeRegistration> {
    let output = neige_git_command()
        .arg("-C")
        .arg(&target.repo_root)
        .args(["worktree", "list", "--porcelain", "-z"])
        .output()
        .map_err(|e| {
            CalmError::Internal(format!(
                "spawn git worktree list in {}: {e}",
                target.repo_root.display()
            ))
        })?;
    if !output.status.success() {
        return Err(git_failed("git worktree list", &target.repo_root, &output));
    }
    base::worktree_registration_in(&output.stdout, target)
}

/// Drop registrations whose directory is gone; touches no files.
fn git_worktree_prune(repo_root: &Path) -> Result<()> {
    let output = neige_git_command()
        .arg("-C")
        .arg(repo_root)
        .args(["worktree", "prune", "--expire", "now"])
        .output()
        .map_err(|e| {
            CalmError::Internal(format!(
                "spawn git worktree prune in {}: {e}",
                repo_root.display()
            ))
        })?;
    if !output.status.success() {
        return Err(git_failed("git worktree prune", repo_root, &output));
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
