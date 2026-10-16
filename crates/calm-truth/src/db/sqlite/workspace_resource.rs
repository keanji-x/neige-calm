//! Workspace identity and binding transactions. Filesystem adoption belongs to the kernel.

use sqlx::{Sqlite, SqliteConnection, Transaction};
use workspace::identity::{WorkspaceCwd, WorkspaceId, WorkspaceRef};

use crate::{
    error::{CalmError, Result},
    model::{TrackWorkspace, TrackWorkspaceKind, new_id, now_ms},
};

#[derive(Clone, Debug)]
pub struct WorkspaceAdoption {
    pub track_id: String,
    pub workspace_id: WorkspaceId,
    pub admission_version: i64,
    pub selected_path: String,
    pub ownership: WorkspaceOwnership,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceOwnership {
    Borrowed,
    Managed,
    TrackWorktree,
}

pub async fn workspace_adoption_snapshot(
    conn: &mut SqliteConnection,
    track_id: &str,
) -> Result<WorkspaceAdoption> {
    let (id, admission_version, selected_path, ownership): (String, i64, String, String) =
        sqlx::query_as(
            "SELECT r.id,b.admission_version,b.selected_path, \
             CASE WHEN t.workspace_worktree_path IS NOT NULL THEN 'track_worktree' \
                  WHEN t.workspace_kind='managed' THEN 'managed' ELSE 'borrowed' END \
         FROM track_workspace_bindings b JOIN workspace_resources r ON r.id=b.workspace_id \
         JOIN tracks t ON t.id=b.track_id \
         WHERE b.track_id=?1 AND b.selected_path=coalesce(t.workspace_worktree_path,t.workspace_path) \
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
    Ok(WorkspaceAdoption {
        track_id: track_id.into(),
        workspace_id: WorkspaceId::try_from(id)
            .map_err(|error| CalmError::Internal(error.to_string()))?,
        admission_version,
        selected_path,
        ownership: match ownership.as_str() {
            "borrowed" => WorkspaceOwnership::Borrowed,
            "managed" => WorkspaceOwnership::Managed,
            "track_worktree" => WorkspaceOwnership::TrackWorktree,
            _ => {
                return Err(CalmError::Internal(
                    "invalid persisted workspace ownership".to_string(),
                ));
            }
        },
    })
}

/// CAS a kernel-probed physical identity. No filesystem work occurs in this transaction.
pub async fn workspace_adopt_tx(
    tx: &mut Transaction<'_, Sqlite>,
    snapshot: &WorkspaceAdoption,
    identity: &workspace::directory::DirectoryIdentity,
) -> Result<()> {
    let current = workspace_adoption_snapshot(tx, &snapshot.track_id).await?;
    if current.workspace_id != snapshot.workspace_id
        || current.admission_version != snapshot.admission_version
        || current.selected_path != snapshot.selected_path
        || current.ownership != snapshot.ownership
    {
        return Err(CalmError::Conflict(
            "workspace selection changed during adoption; retry",
        ));
    }
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
    let registered: Option<(i64, i64, String, String)> = sqlx::query_as(
        "SELECT c.directory_device,c.directory_inode,c.canonical_root,c.git_common_dir \
         FROM workspace_resources r JOIN workspace_resources c ON c.id=r.adopted_workspace_id \
         WHERE r.id=?1",
    )
    .bind(snapshot.workspace_id.as_str())
    .fetch_optional(&mut **tx)
    .await?;
    if let Some((device, inode, registered_root, registered_common)) = registered {
        if device != identity.device()
            || inode != identity.inode()
            || registered_root != root
            || registered_common != common_dir
        {
            return Err(CalmError::Conflict(
                "WorkspaceRecoveryRequired: registered workspace directory was replaced",
            ));
        }
    }
    let existing: Option<(String, String, String)> = sqlx::query_as(
        "SELECT id,canonical_root,git_common_dir FROM workspace_resources \
         WHERE directory_device=?1 AND directory_inode=?2",
    )
    .bind(identity.device())
    .bind(identity.inode())
    .fetch_optional(&mut **tx)
    .await?;
    let canonical = if let Some((id, original_root, original_common)) = existing {
        if original_root != root || original_common != common_dir {
            return Err(CalmError::Conflict(
                "WorkspaceRecoveryRequired: registered directory facts changed",
            ));
        }
        id
    } else {
        snapshot.workspace_id.to_string()
    };
    let previous: Option<String> =
        sqlx::query_scalar("SELECT adopted_workspace_id FROM workspace_resources WHERE id=?1")
            .bind(snapshot.workspace_id.as_str())
            .fetch_one(&mut **tx)
            .await?;
    if previous
        .as_deref()
        .is_some_and(|previous| previous != canonical)
    {
        return Err(CalmError::Conflict(
            "WorkspaceRecoveryRequired: workspace root was replaced",
        ));
    }
    // An old active source proof cannot silently follow a re-point or a different root.
    let mismatched_lease: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM workspace_leases wl JOIN track_workspace_bindings b \
         ON b.track_id=wl.track_id WHERE b.workspace_id=?1 AND wl.state IN ('held','releasing') \
         AND wl.canonical_path IS NOT NULL AND wl.canonical_path<>?2)",
    )
    .bind(snapshot.workspace_id.as_str())
    .bind(root)
    .fetch_one(&mut **tx)
    .await?;
    if mismatched_lease {
        return Err(CalmError::Conflict(
            "WorkspaceRecoveryRequired: an active lease names a different source root",
        ));
    }
    let current_owner: Option<String> =
        sqlx::query_scalar("SELECT physical_owner_track_id FROM workspace_resources WHERE id=?1")
            .bind(&canonical)
            .fetch_one(&mut **tx)
            .await?;
    if let (Some(current_owner), Some(expected_owner)) = (current_owner.as_deref(), expected_owner)
    {
        if current_owner != expected_owner {
            return Err(CalmError::Conflict(
                "WorkspaceRecoveryRequired: physical workspace has another owner",
            ));
        }
    }
    if canonical == snapshot.workspace_id.as_str() {
        sqlx::query(
            "UPDATE workspace_resources SET adopted_workspace_id=id,canonical_root=?2, \
             directory_device=?3,directory_inode=?4,git_common_dir=?5 WHERE id=?1",
        )
        .bind(&canonical)
        .bind(root)
        .bind(identity.device())
        .bind(identity.inode())
        .bind(common_dir)
        .execute(&mut **tx)
        .await?;
    } else {
        sqlx::query("UPDATE workspace_resources SET adopted_workspace_id=?2 WHERE id=?1")
            .bind(snapshot.workspace_id.as_str())
            .bind(&canonical)
            .execute(&mut **tx)
            .await?;
    }
    if let Some(owner) = expected_owner {
        sqlx::query("UPDATE workspace_resources SET physical_owner_track_id=?2 WHERE id=?1")
            .bind(&canonical)
            .bind(owner)
            .execute(&mut **tx)
            .await?;
    }
    if previous.is_none() || canonical != snapshot.workspace_id.as_str() {
        sqlx::query(
            "UPDATE track_workspace_bindings SET cwd=?2,admission_version=admission_version+1,workspace_id=?3 \
             WHERE workspace_id=?1",
        )
        .bind(snapshot.workspace_id.as_str())
        .bind(identity.cwd().as_str())
        .bind(&canonical)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

pub async fn workspace_require_adopted(conn: &mut SqliteConnection, track_id: &str) -> Result<()> {
    track_workspace_reference(conn, track_id).await?;
    Ok(())
}

/// Called only by the four-column workspace writer, after its freeze predicate succeeds.
pub(super) async fn workspace_bind_selection_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &str,
    workspace: &TrackWorkspace,
) -> Result<()> {
    let root = workspace.worktree.as_deref().unwrap_or(&workspace.path);
    let owned = workspace.worktree.is_some() || workspace.kind == TrackWorkspaceKind::Managed;
    let selection = if owned {
        serde_json::json!(["owned", track_id, root])
    } else if root.is_empty() {
        serde_json::json!(["unresolved", track_id])
    } else {
        serde_json::json!(["borrowed", root])
    }
    .to_string();
    sqlx::query(
        "INSERT INTO workspace_resources \
         (id, descriptor_version, selection_key, root_path, ownership, selection_version, \
          generation, installation_epoch, materialization_state, created_at_ms) \
         VALUES (?1, 1, ?2, ?3, ?4, 1, 1, 1, 'pending', ?5) \
         ON CONFLICT(selection_key) DO NOTHING",
    )
    .bind(new_id())
    .bind(&selection)
    .bind(root)
    .bind(if owned { "owned" } else { "borrowed" })
    .bind(now_ms())
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO track_workspace_bindings \
         (track_id, workspace_id, admission_version, selected_path, ownership, cwd, frozen_at_ms) \
         SELECT ?1, id, 1, ?4, ?5, '.', ?3 FROM workspace_resources WHERE selection_key = ?2 \
         ON CONFLICT(track_id) DO UPDATE SET \
         workspace_id = excluded.workspace_id, \
         admission_version = track_workspace_bindings.admission_version + \
             (track_workspace_bindings.workspace_id <> excluded.workspace_id), \
         cwd = CASE WHEN track_workspace_bindings.workspace_id = excluded.workspace_id \
             THEN track_workspace_bindings.cwd ELSE excluded.cwd END, \
         selected_path = excluded.selected_path, ownership = excluded.ownership, \
         frozen_at_ms = excluded.frozen_at_ms",
    )
    .bind(track_id)
    .bind(selection)
    .bind(workspace.frozen_at)
    .bind(root)
    .bind(if owned { "owned" } else { "borrowed" })
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Read the persisted selection. Missing binding is recovery, never path-based authorization.
pub async fn track_workspace_reference(
    conn: &mut SqliteConnection,
    track_id: &str,
) -> Result<WorkspaceRef> {
    let (id, cwd): (String, String) = sqlx::query_as(
        "SELECT b.workspace_id,b.cwd FROM track_workspace_bindings b JOIN workspace_resources r \
            ON r.id=b.workspace_id JOIN tracks t ON t.id=b.track_id WHERE b.track_id=?1 \
            AND r.adopted_workspace_id=r.id AND r.canonical_root IS NOT NULL \
            AND b.selected_path=coalesce(t.workspace_worktree_path,t.workspace_path)",
    )
    .bind(track_id)
    .fetch_optional(conn)
    .await?
    .ok_or_else(|| {
        CalmError::Conflict(format!(
            "WorkspaceRecoveryRequired: track {track_id} has no workspace binding"
        ))
    })?;
    Ok(WorkspaceRef {
        workspace_id: WorkspaceId::try_from(id)
            .map_err(|error| CalmError::Internal(error.to_string()))?,
        cwd: WorkspaceCwd::try_from(cwd).map_err(|error| CalmError::Internal(error.to_string()))?,
    })
}
