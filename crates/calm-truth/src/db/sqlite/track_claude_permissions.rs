//! #1704 S2 — the root-resolving read of a track tree's Claude Code
//! permission policy.
//!
//! The policy lives on the tree ROOT only (`track_update_tx` refuses it on a
//! child), so the ceiling of any track is its root's column. Both readers —
//! the terminal adapter's `prepare_tx` (inside the write transaction) and the
//! handler's pre-check (`RepoRead::track_claude_permissions_ceiling`) — go
//! through this one function, on whatever connection they hold.

use sqlx::SqliteConnection;

use calm_types::claude_permissions::ClaudePermissionsScope;

use super::track_tree::{MAX_TRACK_TREE_DEPTH, TRACK_ROOT_DEPTH_SQL};
use crate::error::{CalmError, Result};

/// The policy that applies to `track_id`: its tree root's
/// `claude_permissions_policy`, `None` when the root carries none.
///
/// Fails closed the way `track_tree_term` does: the bounded ancestor walk
/// must yield exactly one root within [`MAX_TRACK_TREE_DEPTH`] (a missing
/// track, a broken parent link, a cycle or an over-deep chain is a
/// `Conflict`, never "no ceiling"), and a stored value that does not decode
/// as a scope is an error, never "no ceiling".
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
