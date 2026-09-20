//! Root-resolving read of a track tree's Claude Code permission policy: the
//! policy lives on the tree ROOT only, so any track's ceiling is its root's column.

use sqlx::SqliteConnection;

use calm_types::claude_permissions::ClaudePermissionsScope;

use super::track_tree::{MAX_TRACK_TREE_DEPTH, TRACK_ROOT_DEPTH_SQL};
use crate::error::{CalmError, Result};

/// The tree root's `claude_permissions_policy`, `None` when the root carries
/// none. Fails closed: an unresolved root or an undecodable value is an error,
/// never "no ceiling".
pub async fn track_claude_permissions_ceiling_read(
    conn: &mut SqliteConnection,
    track_id: &str,
) -> Result<Option<ClaudePermissionsScope>> {
    let roots: Vec<(String, i64)> = sqlx::query_as(TRACK_ROOT_DEPTH_SQL)
        .bind(track_id)
        .bind(MAX_TRACK_TREE_DEPTH + 1)
        .fetch_all(&mut *conn)
        .await?;
    let root_id = match roots.as_slice() {
        [(root_id, depth)] if *depth <= MAX_TRACK_TREE_DEPTH => root_id.clone(),
        _ => {
            return Err(CalmError::Conflict(format!(
                "track tree root unresolved for {track_id}"
            )));
        }
    };
    let stored: Option<(Option<String>,)> =
        sqlx::query_as("SELECT claude_permissions_policy FROM tracks WHERE id = ?1")
            .bind(&root_id)
            .fetch_optional(&mut *conn)
            .await?;
    let Some((stored,)) = stored else {
        return Err(CalmError::Conflict(format!(
            "track tree root unresolved for {track_id}"
        )));
    };
    stored
        .as_deref()
        .map(serde_json::from_str::<ClaudePermissionsScope>)
        .transpose()
        .map_err(|error| {
            CalmError::Internal(format!(
                "track {root_id} claude_permissions_policy does not decode as a scope: {error}"
            ))
        })
}
