//! Move-only task capabilities backed by durable Track checkout leases.
use super::*;
use crate::db::sqlite::begin_immediate_tx;
use calm_types::workspace_access::WorkspaceAccess;
use std::time::Duration;

pub(crate) enum TaskWorkspaceGuard {
    Read(ReadTaskGuard),
    Write(WriteTaskGuard),
}
pub(crate) struct ReadTaskGuard {
    lease: WorkspaceLease,
    branch: String,
}
pub(crate) struct WriteTaskGuard {
    lease: WorkspaceLease,
}
impl TaskWorkspaceGuard {
    pub(crate) async fn restore(pool: &SqlitePool, id: &str) -> Result<Self> {
        let mut tx = begin_immediate_tx(pool).await?;
        let lease = facts::workspace_lease_by_id_tx(&mut tx, id)
            .await?
            .ok_or_else(|| CalmError::Conflict("workspace guard lease is missing".into()))?;
        if lease.state != "held" {
            return Err(CalmError::Conflict(
                "workspace guard lease is not held".into(),
            ));
        }
        let guard = match lease.access_mode {
            WorkspaceAccess::ReadOnly => {
                let branch = worker_branch_tx(&mut tx, &lease.track_id).await?;
                Self::Read(ReadTaskGuard { lease, branch })
            }
            WorkspaceAccess::ReadWrite => Self::Write(WriteTaskGuard { lease }),
        };
        tx.commit().await?;
        Ok(guard)
    }
    pub(crate) async fn into_sandbox(self) -> Result<&'static str> {
        match self {
            Self::Read(guard) => {
                guard.verify().await?;
                Ok("read-only")
            }
            Self::Write(guard) => {
                if guard.lease.state != "held" {
                    return Err(CalmError::Conflict("write guard was released".into()));
                }
                Ok("workspace-write")
            }
        }
    }
}
impl ReadTaskGuard {
    async fn verify(self) -> Result<()> {
        let base = self
            .lease
            .base
            .as_ref()
            .ok_or_else(|| CalmError::Conflict("read guard has no pinned base".into()))?;
        if std::fs::canonicalize(&self.lease.path)
            .map_err(|error| CalmError::Conflict(error.to_string()))?
            != base.canonical_path
            || read_git(Path::new(&self.lease.path), &["rev-parse", "HEAD"]).await?
                != base.base_sha.as_bytes()
            || read_git(
                Path::new(&self.lease.path),
                &["symbolic-ref", "--short", "HEAD"],
            )
            .await?
                != self.branch.as_bytes()
        {
            return Err(CalmError::Conflict(
                "read guard checkout identity or version changed".into(),
            ));
        }
        verify_read_tree(Path::new(&self.lease.path)).await
    }
}
/// Read probes cannot execute repository-selected filters or fsmonitor helpers.
pub(crate) async fn verify_read_tree(path: &Path) -> Result<()> {
    let keys = read_git(path, &["config", "--null", "--name-only", "--list"]).await?;
    if keys.split(|byte| *byte == 0).any(|key| {
        key.starts_with(b"filter.") && (key.ends_with(b".clean") || key.ends_with(b".process"))
    }) {
        return Err(CalmError::Conflict(
            "read checkout uses unsupported Git filters".into(),
        ));
    }
    let tree = read_git(path, &["ls-tree", "-r", "-z", "HEAD"]).await?;
    if tree
        .split(|byte| *byte == 0)
        .any(|entry| entry.starts_with(b"160000 "))
    {
        return Err(CalmError::Conflict(
            "read checkout uses unsupported submodules".into(),
        ));
    }
    if !read_git(
        path,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=all",
        ],
    )
    .await?
    .is_empty()
    {
        return Err(CalmError::Conflict(
            "read checkout has uncommitted changes".into(),
        ));
    }
    Ok(())
}
async fn read_git(path: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let mut command = std::process::Command::new("/usr/bin/bwrap");
    command
        .env_clear()
        .envs([("PATH", "/usr/bin:/bin"), ("LANG", "C"), ("LC_ALL", "C")])
        .args([
            "--ro-bind",
            "/",
            "/",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--unshare-all",
            "--new-session",
            "--die-with-parent",
            "--",
            "/usr/bin/git",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
            "--no-optional-locks",
            "-C",
        ])
        .arg(path)
        .args(args);
    let output = crate::plugin_host::child_process::run_bounded(
        command.into(),
        tokio::time::Instant::now() + Duration::from_secs(10),
        1024 * 1024,
    )
    .await
    .map_err(|error| CalmError::Conflict(format!("read checkout probe failed: {error:?}")))?;
    if !output.status.success() {
        return Err(CalmError::Conflict("read checkout probe failed".into()));
    }
    let mut bytes = output.stdout;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    Ok(bytes)
}
pub(crate) async fn is_read_card(pool: &SqlitePool, card: &str) -> Result<bool> {
    Ok(sqlx::query_scalar(
        r#"
        SELECT EXISTS(SELECT 1 FROM workspace_leases WHERE card_id=?1 AND access_mode='read_only')
    "#,
    )
    .bind(card)
    .fetch_one(pool)
    .await?)
}
pub(crate) fn turn_stopped(
    facts: &calm_provider::provider::CodexLivenessFacts,
    active: Option<&str>,
) -> bool {
    use calm_provider::provider::{ThreadStatusLite, TurnStatusLite};
    matches!(
        facts.status,
        ThreadStatusLite::Idle | ThreadStatusLite::SystemError
    ) && active.is_none()
        && facts.last_turn.is_some_and(|turn| {
            matches!(
                turn.status,
                TurnStatusLite::Completed | TurnStatusLite::Failed | TurnStatusLite::Interrupted
            )
        })
}
pub(crate) async fn confirm_read_stop(
    pool: &SqlitePool,
    card: &str,
    shared: Option<&crate::shared_codex_appserver::SharedCodexAppServer>,
) -> Result<()> {
    if !is_read_card(pool, card).await? {
        return Ok(());
    }
    let stopped: bool = sqlx::query_scalar(
        r#"
        SELECT EXISTS(SELECT 1 FROM workspace_leases WHERE card_id=?1 AND access_mode='read_only'
          AND read_stop_confirmed_at_ms IS NOT NULL AND state='released')
    "#,
    )
    .bind(card)
    .fetch_one(pool)
    .await?;
    if stopped {
        return Ok(());
    }
    let thread:Option<String>=sqlx::query_scalar(r#"
        SELECT s.thread_id FROM workspace_leases l JOIN worker_sessions s ON s.spawn_op_id=l.lease_owner
        WHERE l.card_id=?1 AND l.access_mode='read_only' ORDER BY l.created_at_ms DESC LIMIT 1
    "#).bind(card).fetch_optional(pool).await?.flatten();
    let shared =
        shared.ok_or_else(|| CalmError::Conflict("read stop requires its daemon".into()))?;
    let thread =
        thread.ok_or_else(|| CalmError::Conflict("read stop requires its thread".into()))?;
    let facts = tokio::time::timeout(
        Duration::from_secs(25),
        calm_provider::provider::CodexDaemonProbe::read_liveness_facts(shared, &thread),
    )
    .await
    .map_err(|_| CalmError::Conflict("read stop probe timed out".into()))?
    .ok_or_else(|| CalmError::Conflict("read stop probe was inconclusive".into()))?;
    if !turn_stopped(&facts, shared.active_turn_id_for_thread(&thread).as_deref()) {
        return Err(CalmError::Conflict(
            "read task has not stopped; lease retained".into(),
        ));
    }
    let background_stopped = tokio::time::timeout(
        Duration::from_secs(25),
        shared.background_terminals_stopped(&thread),
    )
    .await;
    if !matches!(background_stopped, Ok(Ok(true))) {
        return Err(CalmError::Conflict(
            "read task background terminals have not stopped; lease retained".into(),
        ));
    }
    record_read_stop(pool, card).await
}
pub(crate) async fn record_read_stop(pool: &SqlitePool, card: &str) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE workspace_leases SET read_stop_confirmed_at_ms=?1
        WHERE card_id=?2 AND access_mode='read_only' AND state IN ('held','releasing')
    "#,
    )
    .bind(now_ms())
    .bind(card)
    .execute(pool)
    .await?;
    Ok(())
}

/// Admission uses the prepare lease before the scheduler has stamped worker_card_id.
pub(crate) enum PreparedTaskAccess {
    Read,
    Write { attempt: String },
    Independent,
}
pub(crate) async fn prepared_task_access(
    pool: &SqlitePool,
    card: &str,
) -> Result<PreparedTaskAccess> {
    let row=sqlx::query_as::<_,(String,String,Option<String>)>(
        "SELECT l.access_mode,l.state,o.idempotency_key FROM workspace_leases l \
         LEFT JOIN operations o ON o.id=l.lease_owner \
         WHERE l.card_id=?1 AND l.holder_kind='task' ORDER BY l.created_at_ms DESC,l.lease_id DESC LIMIT 1"
    ).bind(card).fetch_optional(pool).await?;
    match row {
        Some((mode, state, _)) if mode == "read_only" => {
            if state != "held" {
                return Err(CalmError::Conflict(
                    "read task guard is no longer held".into(),
                ));
            }
            Ok(PreparedTaskAccess::Read)
        }
        Some((_, state, attempt)) if state == "held" => Ok(PreparedTaskAccess::Write {
            attempt: attempt.ok_or_else(|| {
                CalmError::Conflict("task write guard has no owning attempt".into())
            })?,
        }),
        _ => Ok(PreparedTaskAccess::Independent),
    }
}

/// A card's persistent native references survive projection cleanup until their exact requests stop.
pub(crate) async fn cancel_native_references(
    repo: &dyn crate::db::RouteRepo,
    card: &str,
    shared: Option<&crate::shared_codex_appserver::SharedCodexAppServer>,
) -> Result<()> {
    let card = card.to_owned();
    let references: Vec<String> =
        crate::db::write_in_tx_typed(repo, move |tx| {
            Box::pin(async move {
                Ok(sqlx::query_scalar(
                "SELECT lease_id FROM workspace_leases WHERE card_id=?1 AND holder_kind='native' \
                 AND native_provider='codex' AND state='held' ORDER BY created_at_ms,lease_id",
            ).bind(card).fetch_all(&mut **tx).await?)
            })
        })
        .await?;
    if references.is_empty() {
        return Ok(());
    }
    let shared =
        shared.ok_or_else(|| CalmError::Conflict("native stop requires its provider".into()))?;
    for reference in references {
        match tokio::time::timeout(
            Duration::from_secs(25),
            shared.cancel_native_workspace_guard(&reference),
        )
        .await
        {
            Ok(Ok(true)) => {}
            Ok(Err(error)) => return Err(error),
            Ok(Ok(false)) => {
                return Err(CalmError::Conflict(
                    "native execution stop is unconfirmed; references and workspace retained"
                        .into(),
                ));
            }
            Err(_) => {
                return Err(CalmError::Conflict(
                    "native execution stop timed out; references and workspace retained".into(),
                ));
            }
        }
    }

    Ok(())
}
