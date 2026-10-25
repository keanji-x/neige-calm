//! Owned execution locations, including removals whose live runtime may have disappeared.
use crate::error::Result;
use std::path::PathBuf;

pub struct OwnedWorkspaceLocation {
    pub track_id: String,
    pub path: PathBuf,
    /// A track lock cannot stand in for durable settlement after a lost wait or restart.
    pub removing: bool,
}

/// The durable part of the shared owned/teardown target set. Callers append their live
/// teardown snapshot; writer admission reads this inventory in its own transaction.
/// Reclaim freezes the target before deleting owner rows and commits its execution id
/// with execution admission. Only durable execution settlement releases that target.
pub async fn owned_workspace_locations(
    conn: &mut sqlx::SqliteConnection,
) -> Result<Vec<OwnedWorkspaceLocation>> {
    let mut locations = super::track_worktree_locations(conn)
        .await?
        .into_iter()
        .map(|(track_id, path)| OwnedWorkspaceLocation {
            track_id,
            path,
            removing: false,
        })
        .collect::<Vec<_>>();
    let removing: Vec<(String, String)> = sqlx::query_as(
        "SELECT json_extract(o.payload_json,'$.target.track_id'), \
                json_extract(o.payload_json,'$.target.request.path') \
         FROM operations o JOIN workspace_executions e \
           ON e.execution_id = json_extract(o.tx_output_json,'$.data.workspace_reclaim.execution_id') \
         WHERE o.kind = 'workspace-reclaim' \
           AND json_extract(e.intent_json,'$.consumer') = 'workspace-reclaim' \
           AND e.settled_at_ms IS NULL",
    )
    .fetch_all(conn)
    .await?;
    locations.extend(
        removing
            .into_iter()
            .map(|(track_id, path)| OwnedWorkspaceLocation {
                track_id,
                path: path.into(),
                removing: true,
            }),
    );
    Ok(locations)
}
