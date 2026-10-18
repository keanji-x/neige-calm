//! Logical workspace selection bindings and current directory observations.
//! An observation cannot prove physical object identity across lifetimes.

use sqlx::{Sqlite, SqliteConnection, Transaction};
use workspace::identity::{WorkspaceCwd, WorkspaceId};

use crate::{
    error::{CalmError, Result},
    model::{TrackWorkspace, TrackWorkspaceKind, new_id, now_ms},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceObservation {
    pub track_id: String,
    pub workspace_id: WorkspaceId,
    pub selection_version: i64,
    pub selected_path: String,
    pub ownership: WorkspaceOwnership,
    pub state: DirectoryObservation,
}

impl WorkspaceObservation {
    /// Whether this exact persisted observation describes the retained directory proof.
    pub fn matches_identity(&self, identity: &workspace::directory::DirectoryIdentity) -> bool {
        matches!(&self.state, DirectoryObservation::Observed(observed)
            if observed.root == identity.root().to_str().expect("validated workspace UTF-8 root")
                && observed.cwd == *identity.cwd()
                && observed.common_dir == identity.common_dir().to_str().expect("validated Git common directory")
                && observed.device == identity.device() && observed.inode == identity.inode()
                && observed.owner.as_deref() == identity.owner())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DirectoryObservation {
    Pending,
    Observed(ObservedDirectory),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedDirectory {
    pub version: i64,
    pub root: String,
    pub cwd: WorkspaceCwd,
    pub common_dir: String,
    pub device: i64,
    pub inode: i64,
    pub owner: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceOwnership {
    Borrowed,
    Managed,
    TrackWorktree,
}

#[derive(sqlx::FromRow)]
struct ObservationRow {
    workspace_id: String,
    selection_version: i64,
    selected_path: String,
    ownership: String,
    observation_version: i64,
    observed_root: Option<String>,
    observed_cwd: Option<String>,
    directory_device: Option<i64>,
    directory_inode: Option<i64>,
    git_common_dir: Option<String>,
    physical_owner_track_id: Option<String>,
}

pub async fn workspace_observation_snapshot(
    conn: &mut SqliteConnection,
    track_id: &str,
) -> Result<WorkspaceObservation> {
    let row: ObservationRow = sqlx::query_as(
        "SELECT r.id AS workspace_id,b.selection_version,b.selected_path, \
         CASE WHEN t.workspace_worktree_path IS NOT NULL THEN 'track_worktree' \
              WHEN t.workspace_kind='managed' THEN 'managed' ELSE 'borrowed' END AS ownership, \
         r.observation_version,r.observed_root,r.observed_cwd,r.directory_device, \
         r.directory_inode,r.git_common_dir,r.physical_owner_track_id \
         FROM track_workspace_bindings b JOIN workspace_resources r ON r.id=b.workspace_id \
         JOIN tracks t ON t.id=b.track_id WHERE b.track_id=?1 \
         AND r.source_track_id=t.id AND r.root_path=b.selected_path AND r.ownership=b.ownership \
         AND b.selected_path=coalesce(t.workspace_worktree_path,t.workspace_path) \
         AND b.ownership=CASE WHEN t.workspace_worktree_path IS NOT NULL OR t.workspace_kind='managed' \
             THEN 'owned' ELSE 'borrowed' END",
    )
    .bind(track_id)
    .fetch_optional(conn)
    .await?
    .ok_or_else(|| {
        CalmError::Conflict(format!(
            "WorkspaceRecoveryRequired: track {track_id} has no workspace selection"
        ))
    })?;
    let state = match row {
        ObservationRow {
            observation_version: 0,
            observed_root: None,
            observed_cwd: None,
            directory_device: None,
            directory_inode: None,
            git_common_dir: None,
            physical_owner_track_id: None,
            ..
        } => DirectoryObservation::Pending,
        ObservationRow {
            observation_version: version,
            observed_root: Some(ref root),
            observed_cwd: Some(ref cwd),
            directory_device: Some(device),
            directory_inode: Some(inode),
            git_common_dir: Some(ref common_dir),
            ref physical_owner_track_id,
            ..
        } if version > 0 => DirectoryObservation::Observed(ObservedDirectory {
            version,
            root: root.clone(),
            cwd: WorkspaceCwd::try_from(cwd.clone())
                .map_err(|error| CalmError::Internal(error.to_string()))?,
            common_dir: common_dir.clone(),
            device,
            inode,
            owner: physical_owner_track_id.clone(),
        }),
        _ => {
            return Err(CalmError::Internal(
                "invalid workspace observation tuple".into(),
            ));
        }
    };
    Ok(WorkspaceObservation {
        track_id: track_id.into(),
        workspace_id: WorkspaceId::try_from(row.workspace_id)
            .map_err(|error| CalmError::Internal(error.to_string()))?,
        selection_version: row.selection_version,
        selected_path: row.selected_path,
        state,
        ownership: match row.ownership.as_str() {
            "borrowed" => WorkspaceOwnership::Borrowed,
            "managed" => WorkspaceOwnership::Managed,
            "track_worktree" => WorkspaceOwnership::TrackWorktree,
            _ => {
                return Err(CalmError::Internal(
                    "invalid persisted workspace ownership".into(),
                ));
            }
        },
    })
}

/// CAS one selection's observation. No global inode/path lookup or filesystem work occurs here.
pub async fn workspace_observe_tx(
    tx: &mut Transaction<'_, Sqlite>,
    snapshot: &WorkspaceObservation,
    identity: &workspace::directory::DirectoryIdentity,
) -> Result<()> {
    let current = workspace_observation_snapshot(tx, &snapshot.track_id).await?;
    if identity.selected_path() != std::path::Path::new(&snapshot.selected_path) {
        return Err(CalmError::Conflict(
            "workspace directory evidence names another selection",
        ));
    }
    let expected_owner =
        (snapshot.ownership != WorkspaceOwnership::Borrowed).then_some(snapshot.track_id.as_str());
    if identity.owner() != expected_owner {
        return Err(CalmError::Conflict(
            "workspace ownership evidence is missing or targets another owner",
        ));
    }
    let root = identity
        .root()
        .to_str()
        .expect("validated workspace UTF-8 root");
    let common_dir = identity
        .common_dir()
        .to_str()
        .expect("validated Git common directory");
    let selection_changed = current.workspace_id != snapshot.workspace_id
        || current.selection_version != snapshot.selection_version
        || current.selected_path != snapshot.selected_path
        || current.ownership != snapshot.ownership;
    let identical_facts = current.matches_identity(identity);
    if selection_changed || current.state != snapshot.state {
        // Converge only a completed identical observation of this exact selection. An old
        // selection or different probe must never overwrite a newer binding/observation.
        if !selection_changed && identical_facts {
            return Ok(());
        }
        return Err(CalmError::Conflict(
            "workspace selection changed during observation; retry",
        ));
    }
    if identical_facts {
        return Ok(());
    }
    let version = match &snapshot.state {
        DirectoryObservation::Pending => 1,
        DirectoryObservation::Observed(previous) => {
            if previous.root != root
                || previous.cwd != *identity.cwd()
                || previous.common_dir != common_dir
            {
                return Err(CalmError::Conflict(
                    "WorkspaceRecoveryRequired: observed selection target or cwd changed",
                ));
            }
            previous.version.checked_add(1).ok_or_else(|| {
                CalmError::Internal("workspace observation version exhausted".into())
            })?
        }
    };
    sqlx::query(
        "UPDATE workspace_resources SET observation_version=?2,observed_root=?3,observed_cwd=?4, \
         directory_device=?5,directory_inode=?6,git_common_dir=?7,physical_owner_track_id=?8 WHERE id=?1",
    )
    .bind(snapshot.workspace_id.as_str()).bind(version).bind(root).bind(identity.cwd().as_str())
    .bind(identity.device()).bind(identity.inode()).bind(common_dir).bind(expected_owner)
    .execute(&mut **tx).await?;
    Ok(())
}

/// Called only by the four-column workspace writer, after its freeze predicate succeeds.
pub(super) async fn workspace_bind_selection_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &str,
    workspace: &TrackWorkspace,
) -> Result<()> {
    let root = workspace.worktree.as_deref().unwrap_or(&workspace.path);
    let ownership = if workspace.worktree.is_some() || workspace.kind == TrackWorkspaceKind::Managed
    {
        "owned"
    } else {
        "borrowed"
    };
    let unchanged = sqlx::query(
        "UPDATE track_workspace_bindings SET frozen_at_ms=?4 \
         WHERE track_id=?1 AND selected_path=?2 AND ownership=?3",
    )
    .bind(track_id)
    .bind(root)
    .bind(ownership)
    .bind(workspace.frozen_at)
    .execute(&mut **tx)
    .await?;
    if unchanged.rows_affected() == 1 {
        return Ok(());
    }
    let id = new_id();
    sqlx::query(
        "INSERT INTO workspace_resources \
         (id,source_track_id,root_path,ownership,observation_version,created_at_ms) VALUES (?1,?2,?3,?4,0,?5)",
    ).bind(&id).bind(track_id).bind(root).bind(ownership).bind(now_ms())
        .execute(&mut **tx).await?;
    sqlx::query(
        "INSERT INTO track_workspace_bindings \
         (track_id,workspace_id,selection_version,selected_path,ownership,frozen_at_ms) VALUES (?1,?2,1,?3,?4,?5) \
         ON CONFLICT(track_id) DO UPDATE SET workspace_id=excluded.workspace_id, \
         selection_version=track_workspace_bindings.selection_version+1, \
         selected_path=excluded.selected_path,ownership=excluded.ownership,frozen_at_ms=excluded.frozen_at_ms",
    ).bind(track_id).bind(&id).bind(root).bind(ownership).bind(workspace.frozen_at)
        .execute(&mut **tx).await?;
    Ok(())
}
