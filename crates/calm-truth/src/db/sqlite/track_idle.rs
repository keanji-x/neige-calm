//! Actual directory admission shared by task claims, native calls and pending diagnostics.
use crate::error::{CalmError, Result};
use crate::model::{Task, TaskKind};
use calm_types::workspace_access::WorkspaceAccess;
use sqlx::SqliteConnection;
use std::path::{Component, Path, PathBuf};

/// Only terminal workers use their declared cwd; native task workers use the Track checkout.
pub fn task_workspace_cwd(task: &Task) -> Option<&str> {
    declared_workspace_cwd(task.kind == TaskKind::Terminal, task.cwd.as_deref())
}
pub(super) fn declared_workspace_cwd(terminal: bool, cwd: Option<&str>) -> Option<&str> {
    terminal
        .then_some(cwd)
        .flatten()
        .map(str::trim)
        .filter(|cwd| !cwd.is_empty())
}

pub async fn track_idle(conn: &mut SqliteConnection, track: &str, except: &str) -> Result<bool> {
    track_available(conn, track, except, WorkspaceAccess::ReadWrite).await
}
pub async fn track_available(
    conn: &mut SqliteConnection,
    track: &str,
    except: &str,
    access: WorkspaceAccess,
) -> Result<bool> {
    workspace_available(conn, track, except, access, None, None).await
}
pub async fn workspace_available(
    conn: &mut SqliteConnection,
    track: &str,
    except: &str,
    access: WorkspaceAccess,
    cwd: Option<&str>,
    write_root: Option<&str>,
) -> Result<bool> {
    let declared: Option<String> = sqlx::query_scalar(
        "SELECT COALESCE(workspace_worktree_path,workspace_path) FROM tracks WHERE id=?1",
    )
    .bind(track)
    .fetch_optional(&mut *conn)
    .await?;
    let path = cwd
        .map(str::to_owned)
        .or(declared)
        .ok_or_else(|| CalmError::NotFound(format!("track {track}")))?;
    let scope = Scope {
        track: track.to_owned(),
        path: physical_path(&path)?,
    };
    if access == WorkspaceAccess::ReadOnly && !legacy_native_scopes_available(conn, &scope).await? {
        return Ok(false);
    }
    if !active_resources_available(conn, &scope, except, Some(except), access, write_root).await? {
        return Ok(false);
    }
    deliveries_available(conn, &scope, except, None).await
}

/// Delivery preparation uses its pinned lease scope, not the current Track configuration.
/// Its producer may no longer be an in-flight task, but every live lease still fences commit.
pub async fn delivery_workspace_available(
    conn: &mut SqliteConnection,
    delivery: &str,
) -> Result<bool> {
    let row: Option<(String, String, String, i64)> = sqlx::query_as(
        r#"
SELECT d.track_id,d.producer_attempt_id,COALESCE(l.canonical_path,l.path),d.created_at_ms
FROM task_git_deliveries d JOIN workspace_leases l ON l.lease_id=d.lease_id
WHERE d.delivery_id=?1
"#,
    )
    .bind(delivery)
    .fetch_optional(&mut *conn)
    .await?;
    let (track, producer, path, created) =
        row.ok_or_else(|| CalmError::NotFound(format!("delivery {delivery}")))?;
    let scope = Scope {
        track,
        path: physical_path(&path)?,
    };
    if !active_resources_available(
        conn,
        &scope,
        &producer,
        None,
        WorkspaceAccess::ReadWrite,
        None,
    )
    .await?
    {
        return Ok(false);
    }
    // Different workflows sharing a resource drain in creation order; no delivery waits on later work.
    deliveries_available(conn, &scope, "", Some((created, delivery))).await
}

struct Scope {
    track: String,
    path: Option<PathBuf>,
}
impl Scope {
    fn overlaps(&self, track: &str, path: &str) -> Result<bool> {
        let other = physical_path(path)?;
        Ok(match (&self.path, other) {
            (Some(a), Some(b)) => a.starts_with(&b) || b.starts_with(a),
            _ => self.track == track,
        })
    }
}

/// Resolve existing prefixes as well: a not-yet-created terminal cwd still names a resource.
/// Permission and I/O errors are not absence and must never silently admit a writer.
fn physical_path(path: &str) -> Result<Option<PathBuf>> {
    if path.is_empty() {
        return Ok(None);
    }
    let path = Path::new(path);
    if !path.is_absolute() {
        return Err(CalmError::BadRequest("workspace cwd must be absolute"));
    }
    let mut prefix = path;
    loop {
        match std::fs::canonicalize(prefix) {
            Ok(root) => {
                let suffix = path
                    .strip_prefix(prefix)
                    .map_err(|e| CalmError::Internal(e.to_string()))?;
                let mut normalized = PathBuf::new();
                for component in root.join(suffix).components() {
                    match component {
                        Component::ParentDir => {
                            normalized.pop();
                        }
                        Component::CurDir => {}
                        component => normalized.push(component.as_os_str()),
                    }
                }
                return Ok(Some(normalized));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                prefix = prefix.parent().ok_or_else(|| {
                    CalmError::BadRequest(format!("workspace cwd unavailable: {error}"))
                })?;
            }
            Err(error) => {
                return Err(CalmError::BadRequest(format!(
                    "workspace cwd unavailable: {error}"
                )));
            }
        }
    }
}

#[derive(sqlx::FromRow)]
struct LegacyNativeScope {
    track_id: String,
    card_id: Option<String>,
    provider: String,
    holder_id: String,
    spawn_op_id: Option<String>,
    cwd: Option<String>,
}

/// A legacy provider must establish its actual scope before a reader can be admitted.
/// Missing scope is unknown globally; recovering scope fences only its physical resource.
/// An exact existing execution reference already provides that same admission barrier.
async fn legacy_native_scopes_available(
    conn: &mut SqliteConnection,
    scope: &Scope,
) -> Result<bool> {
    let sessions = sqlx::query_as::<_, LegacyNativeScope>(
        r#"
SELECT s.track_id,s.card_id,s.provider,
CASE WHEN s.provider='codex' THEN s.thread_id ELSE s.id END AS holder_id,
s.spawn_op_id,b.cwd
FROM worker_sessions s
LEFT JOIN workspace_execution_bindings b ON b.provider=s.provider
AND b.holder_id=CASE WHEN s.provider='codex' THEN s.thread_id ELSE s.id END
WHERE s.state IN ('starting','running','idle','turn_pending')
AND ((s.provider='codex' AND s.thread_id IS NOT NULL)
 OR (s.provider='claude' AND s.terminal_run_id IS NULL))
AND (b.scope_phase IS NULL OR b.scope_phase='recovering')
"#,
    )
    .fetch_all(&mut *conn)
    .await?;
    for session in sessions {
        let paths: Vec<(String, String)> = sqlx::query_as(
            r#"
SELECT COALESCE(l.canonical_path,l.path),l.holder_kind FROM workspace_leases l
WHERE l.state IN ('held','releasing') AND l.card_id=?1
AND ((l.holder_kind='native' AND l.native_provider=?2 AND l.holder_id=?3)
 OR (l.holder_kind='task' AND l.lease_owner=?4))
"#,
        )
        .bind(&session.card_id)
        .bind(&session.provider)
        .bind(&session.holder_id)
        .bind(&session.spawn_op_id)
        .fetch_all(&mut *conn)
        .await?;
        let known = session
            .cwd
            .as_deref()
            .map(physical_path)
            .transpose()?
            .flatten();
        let mut covered = false;
        for (path, kind) in paths {
            let held = physical_path(&path)?;
            if (known.is_none() && kind == "task") || (known.is_some() && held == known) {
                covered = true;
                break;
            }
        }
        if covered {
            continue;
        }
        if let Some(cwd) = &session.cwd {
            if !scope.overlaps(&session.track_id, cwd)? {
                continue;
            }
        }
        // No Track checkout guess can substitute for an unobserved native cwd.
        return Ok(false);
    }
    Ok(true)
}

#[derive(sqlx::FromRow)]
struct ActiveTaskResource {
    track_id: String,
    kind: TaskKind,
    context_json: String,
    cwd: Option<String>,
    checkout: String,
    verifying: bool,
    gate_cwd: Option<String>,
    frozen_cwd: Option<String>,
}
async fn active_resources_available(
    conn: &mut SqliteConnection,
    scope: &Scope,
    except_task: &str,
    except_lease_attempt: Option<&str>,
    access: WorkspaceAccess,
    write_root: Option<&str>,
) -> Result<bool> {
    // Only reserved executions are inspected; pending work and unrelated Track trees are not scanned.
    let tasks = sqlx::query_as::<_,ActiveTaskResource>(r#"
SELECT t.track_id,t.kind,t.context_json,t.cwd,COALESCE(w.workspace_worktree_path,w.workspace_path) AS checkout,
(t.status='verifying') AS verifying,json_extract(t.gate_json,'$.cwd') AS gate_cwd,
CASE WHEN t.status='verifying' THEN (
 SELECT json_extract(o.tx_output_json,'$.data.cwd') FROM operations o
 WHERE o.kind='task-verify' AND o.target_type='task' AND o.target_id=t.id
 AND json_extract(o.tx_output_json,'$.data.attempt')=t.gate_attempt
 ORDER BY o.updated_at_ms DESC,o.id DESC LIMIT 1
) ELSE (
 SELECT json_extract(o.tx_output_json,'$.data.cwd') FROM operations o
 WHERE o.idempotency_key=t.id AND o.kind IN ('codex-worker','claude-worker','terminal-worker')
 ORDER BY o.updated_at_ms DESC,o.id DESC LIMIT 1
) END AS frozen_cwd
FROM current_tasks t JOIN tracks w ON w.id=t.track_id
WHERE t.id<>?1 AND t.status IN ('dispatched','running','verifying')
AND (t.kind='terminal' OR (t.kind IN ('codex','claude') AND t.spawn<>?2))
"#).bind(except_task).bind(calm_types::task_recovery::TASK_CHILD_TRACK_ROUTE).fetch_all(&mut *conn).await?;
    for task in tasks {
        let declared = declared_workspace_cwd(task.kind == TaskKind::Terminal, task.cwd.as_deref());
        let gate = task
            .gate_cwd
            .as_deref()
            .filter(|cwd| !cwd.trim().is_empty());
        let path = task
            .frozen_cwd
            .as_deref()
            .filter(|cwd| !cwd.is_empty())
            .or(if task.verifying {
                gate.or(declared)
            } else {
                declared
            })
            .unwrap_or(&task.checkout);
        if !scope.overlaps(&task.track_id, path)? {
            continue;
        }
        let context = serde_json::from_str(&task.context_json)
            .map_err(|e| CalmError::BadRequest(format!("task context: {e}")))?;
        let other = WorkspaceAccess::from_context(&context).map_err(CalmError::BadRequest)?;
        if access == WorkspaceAccess::ReadWrite || other == WorkspaceAccess::ReadWrite {
            return Ok(false);
        }
    }
    let leases: Vec<(String, String)> = sqlx::query_as(
        r#"
SELECT wl.track_id,COALESCE(wl.canonical_path,wl.path)
FROM workspace_leases wl LEFT JOIN operations o ON o.id=wl.lease_owner
WHERE wl.state IN ('held','releasing')
AND (?1 IS NULL OR o.idempotency_key IS NOT ?1)
AND (o.phase IS NOT 'stuck' OR ?2='read_only' OR wl.holder_kind<>'task')
AND (?2='read_write' OR wl.access_mode='read_write')
AND (?3 IS NULL OR ?2='read_only' OR wl.access_mode='read_only' OR wl.write_root_id IS NOT ?3)
"#,
    )
    .bind(except_lease_attempt)
    .bind(match access {
        WorkspaceAccess::ReadOnly => "read_only",
        WorkspaceAccess::ReadWrite => "read_write",
    })
    .bind(write_root)
    .fetch_all(&mut *conn)
    .await?;
    for (track, path) in leases {
        if scope.overlaps(&track, &path)? {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn deliveries_available(
    conn: &mut SqliteConnection,
    scope: &Scope,
    except_producer: &str,
    before: Option<(i64, &str)>,
) -> Result<bool> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        r#"
SELECT d.track_id,COALESCE(l.canonical_path,l.path)
FROM task_git_deliveries d JOIN workspace_leases l ON l.lease_id=d.lease_id
WHERE d.settlement IS NULL AND d.producer_attempt_id<>?1
AND (?2 IS NULL OR d.created_at_ms<?2 OR (d.created_at_ms=?2 AND d.delivery_id<?3))
"#,
    )
    .bind(except_producer)
    .bind(before.map(|(created, _)| created))
    .bind(before.map(|(_, id)| id))
    .fetch_all(&mut *conn)
    .await?;
    for (track, path) in rows {
        if scope.overlaps(&track, &path)? {
            return Ok(false);
        }
    }
    Ok(true)
}
