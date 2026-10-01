//! #1830 S2 D5: whether a track's checkout is free for its next codex or claude worker. The
//! scheduler's claim and the `trackBusy` pending reason read this one predicate.

use sqlx::SqliteConnection;

use super::task::TASK_COLUMNS;
use crate::error::Result;
use crate::model::Task;

/// No attempt other than `except_attempt` is using the track's checkout. Three terms, each
/// covering what the others miss: no in-tree worker task is `dispatched`/`running`/`verifying`
/// (a gate reading the tree after the release; a claim before its lease exists); no lease is
/// `held`/`releasing` unless its owner op is `stuck` (a canceled worker not yet killed); no
/// delivery is unsettled (a commit not yet landed).
pub async fn track_idle(
    conn: &mut SqliteConnection,
    track_id: &str,
    except_attempt: &str,
) -> Result<bool> {
    track_available(
        conn,
        track_id,
        except_attempt,
        calm_types::workspace_access::WorkspaceAccess::ReadWrite,
    )
    .await
}

pub async fn track_available(
    conn: &mut SqliteConnection,
    track_id: &str,
    except_attempt: &str,
    access: calm_types::workspace_access::WorkspaceAccess,
) -> Result<bool> {
    workspace_available(conn,track_id,except_attempt,access,None,None).await
}

pub async fn workspace_available(
    conn:&mut SqliteConnection,track_id:&str,except_attempt:&str,
    access:calm_types::workspace_access::WorkspaceAccess,cwd:Option<&str>,write_root:Option<&str>,
)->Result<bool> {
    let sql = format!(
        "SELECT {TASK_COLUMNS} FROM current_tasks WHERE track_id = ?1 AND id <> ?2 \
         AND status IN ('dispatched','running','verifying')"
    );
    let in_flight = sqlx::query_as::<_, Task>(&sql)
        .bind(track_id)
        .bind(except_attempt)
        .fetch_all(&mut *conn)
        .await?;
    for task in in_flight {
        if task.runs_in_track_checkout()
            && (access == calm_types::workspace_access::WorkspaceAccess::ReadWrite
                || task
                    .workspace_access()
                    .map_err(crate::error::CalmError::BadRequest)?
                    == calm_types::workspace_access::WorkspaceAccess::ReadWrite)
        {
            return Ok(false);
        }
    }
    let declared: Option<(String, Option<String>)> =
        sqlx::query_as("SELECT workspace_path,workspace_worktree_path FROM tracks WHERE id=?1")
            .bind(track_id)
            .fetch_optional(&mut *conn)
            .await?;
    let requested=cwd.map(str::to_owned).or_else(||declared.map(|(path,worktree)|worktree.unwrap_or(path)));
    let resource = requested
        .and_then(|path|std::fs::canonicalize(path).ok())
        .and_then(|path| path.to_str().map(str::to_owned));
    // `'stuck'` is the operations phase of an owner whose outcome is unknown (`PhaseTag::Stuck`).
    let lease_held: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM workspace_leases wl \
         LEFT JOIN operations o ON o.id = wl.lease_owner \
         WHERE ((?5=1 AND wl.track_id = ?1) OR (?4 IS NOT NULL AND (
         COALESCE(wl.canonical_path,wl.path)=?4 OR COALESCE(wl.canonical_path,wl.path)='/' OR ?4='/'
         OR substr(COALESCE(wl.canonical_path,wl.path),1,length(?4)+1)=?4||'/'
         OR substr(?4,1,length(COALESCE(wl.canonical_path,wl.path))+1)=COALESCE(wl.canonical_path,wl.path)||'/')))
         AND wl.state IN ('held','releasing') \
         AND o.idempotency_key IS NOT ?2 \
         AND (o.phase IS NOT 'stuck' OR ?3='read_only' OR wl.holder_kind<>'task') \
         AND (?3='read_write' OR wl.access_mode='read_write') \
         AND (?6 IS NULL OR wl.write_root_id IS NOT ?6))",
    )
    .bind(track_id)
    .bind(except_attempt)
    .bind(match access {
        calm_types::workspace_access::WorkspaceAccess::ReadOnly => "read_only",
        _ => "read_write",
    })
    .bind(resource.as_deref())
    .bind(cwd.is_none())
    .bind(write_root)
    .fetch_one(&mut *conn)
    .await?;
    if lease_held {
        return Ok(false);
    }
    let delivery_unsettled: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM task_git_deliveries \
         WHERE track_id = ?1 AND settlement IS NULL AND producer_attempt_id <> ?2)",
    )
    .bind(track_id)
    .bind(except_attempt)
    .fetch_one(&mut *conn)
    .await?;
    Ok(!delivery_unsettled)
}
