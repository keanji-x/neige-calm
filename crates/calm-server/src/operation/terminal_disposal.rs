//! Destruction must preserve an unresolved prepared launch, even when a probe currently finds no process.
use super::{Phase, Tx, terminal_launch::RequestState};
use crate::db::{RouteRepo, write_in_tx_typed};
use crate::error::{CalmError, Result};
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub(crate) enum Scope {
    Card(String),
    Terminal(String),
    Track(String),
    Area(String),
}
struct Unresolved {
    operation_id: String,
    terminal_id: String,
    socket: Option<PathBuf>,
}
impl Unresolved {
    fn error(&self) -> CalmError {
        CalmError::Conflict(format!(
            "operation {} has an unresolved launch for terminal {}; rows and workspace retained for reconciliation before deletion or relocation",
            self.operation_id, self.terminal_id
        ))
    }
}

async fn unresolved_tx(tx: &mut Tx<'_>, scope: &Scope) -> Result<Vec<Unresolved>> {
    let (column, id) = match scope {
        Scope::Card(id) => ("c.id", id),
        Scope::Terminal(id) => ("json_extract(o.tx_output_json,'$.data.terminal_id')", id),
        Scope::Track(id) => ("c.track_id", id),
        Scope::Area(id) => ("t.area_id", id),
    };
    // Column is selected exclusively from the static scope above.
    let sql = format!(
        "SELECT o.* FROM operations o JOIN cards c ON (o.target_type='card' AND o.target_id=c.id OR json_extract(o.tx_output_json,'$.data.card_id')=c.id) JOIN tracks t ON t.id=c.track_id WHERE o.kind IN ('terminal-worker','claude-worker','codex-worker','terminal-create','codex-create','claude-create') AND {column}=?1 ORDER BY o.id"
    );
    let rows = sqlx::query(&sql).bind(id).fetch_all(&mut **tx).await?;
    let mut unresolved = Vec::new();
    for row in rows {
        let op = super::repo_sqlite::operation_from_row(&row)?;
        let started = super::worker_cleanup::may_have_started(&op)?;
        if !started {
            continue;
        }
        let output = op.tx_output.as_ref().ok_or_else(|| {
            CalmError::Conflict(format!(
                "operation {} has no prepared launch identity; retain resources for reconciliation",
                op.id
            ))
        })?;
        let terminal_id = output.output_string("terminal_id", "terminal disposal")?;
        let state = RequestState::read(&output.data)?;
        let successful = matches!(op.phase, Phase::Succeeded | Phase::SpawnSucceeded);
        let socket = match state {
            Some(RequestState::Stopped { .. }) => continue,
            Some(RequestState::Requested {
                terminal_id: recorded,
                supervisor_sock,
                ..
            }) => {
                if recorded != terminal_id {
                    return Err(CalmError::Conflict(
                        "prepared launch terminal identity changed; retain resources".into(),
                    ));
                }
                Some(supervisor_sock)
            }
            Some(RequestState::HandedOff {
                terminal_id: recorded,
                supervisor_sock,
                ..
            }) => {
                if recorded != terminal_id {
                    return Err(CalmError::Conflict(
                        "handed off terminal identity changed; retain resources".into(),
                    ));
                }
                if successful || matches!(op.phase, Phase::SpawnStarted) {
                    continue;
                }
                // Compensation after a handed off launch is still unresolved;
                // a group kill acknowledgement never proves disposal safe.
                Some(supervisor_sock)
            }
            Some(RequestState::NotRequested { .. }) if op.kind != "codex-worker" || successful => {
                continue;
            }
            // Old successful operations predate this checkpoint; missing state at SpawnStarted is NOT success.
            None if successful => continue,
            _ => None,
        };
        unresolved.push(Unresolved {
            operation_id: op.id,
            terminal_id,
            socket,
        });
    }
    Ok(unresolved)
}

async fn writers_tx(tx: &mut Tx<'_>, scope: &Scope) -> Result<Vec<(String, Option<PathBuf>)>> {
    let (column, id) = match scope {
        Scope::Card(id) => ("c.id", id),
        Scope::Terminal(id) => ("l.holder_id", id),
        Scope::Track(id) => ("c.track_id", id),
        Scope::Area(id) => ("t.area_id", id),
    };
    let sql = format!(
        r#"
SELECT DISTINCT l.holder_id,json_extract(o.tx_output_json,'$.data.terminal_launch.supervisor_sock')
FROM workspace_leases l JOIN cards c ON c.id=l.card_id JOIN tracks t ON t.id=c.track_id
LEFT JOIN operations o ON json_extract(o.tx_output_json,'$.data.terminal_id')=l.holder_id
WHERE l.holder_kind='terminal' AND l.state IN ('held','releasing') AND {column}=?1
"#
    );
    let rows: Vec<(String, Option<String>)> =
        sqlx::query_as(&sql).bind(id).fetch_all(&mut **tx).await?;
    Ok(rows
        .into_iter()
        .map(|(id, sock)| (id, sock.map(PathBuf::from)))
        .collect())
}

async fn live_business_references_tx(tx: &mut Tx<'_>, scope: &Scope) -> Result<bool> {
    let (column, id) = match scope {
        Scope::Card(id) => ("c.id", id),
        Scope::Track(id) => ("c.track_id", id),
        Scope::Area(id) => ("t.area_id", id),
        Scope::Terminal(_) => return Ok(false),
    };
    let sql = format!(
        "SELECT EXISTS(SELECT 1 FROM workspace_leases l JOIN cards c ON c.id=l.card_id \
         JOIN tracks t ON t.id=c.track_id WHERE l.holder_kind IN ('native','forge') \
         AND l.state IN ('held','releasing') AND {column}=?1)"
    );
    let active: bool = sqlx::query_scalar(&sql)
        .bind(id)
        .fetch_one(&mut **tx)
        .await?;
    Ok(active)
}

/// Transaction recheck immediately before removing rows or replacing workspace ownership; no supervisor I/O happens in this writer.
pub(crate) async fn require_safe_tx(tx: &mut Tx<'_>, scope: &Scope) -> Result<()> {
    if !writers_tx(tx, scope).await?.is_empty() {
        return Err(CalmError::Conflict(
            "terminal writer requires confirmed stop before deletion or relocation".into(),
        ));
    }
    if live_business_references_tx(tx, scope).await? {
        return Err(CalmError::Conflict(
            "execution reference requires confirmed stop before deletion or relocation".into(),
        ));
    }
    if let Some(unresolved) = unresolved_tx(tx, scope).await?.first() {
        return Err(unresolved.error());
    }
    Ok(())
}

/// Inspect the entire affected scope before touching any member's resources; the durable obligation is kept regardless of reply, missing PID, renderer state or a truthful Probe(false).
pub(crate) async fn require_safe(
    repo: &dyn RouteRepo,
    scope: Scope,
    configured_socket: Option<&Path>,
) -> Result<()> {
    let writer_scope = scope.clone();
    let writers = write_in_tx_typed(repo, move |tx| {
        Box::pin(async move { writers_tx(tx, &writer_scope).await })
    })
    .await?;
    for (terminal, sock) in writers {
        let sock = sock.as_deref().or(configured_socket).ok_or_else(|| {
            CalmError::Conflict(
                "terminal writer has no supervisor endpoint; retain resources".into(),
            )
        })?;
        crate::terminal_renderer::stop_and_release_terminal(repo, sock, &terminal).await?;
    }
    let unresolved = write_in_tx_typed(repo, move |tx| {
        Box::pin(async move { unresolved_tx(tx, &scope).await })
    })
    .await?;
    for launch in &unresolved {
        if let Some(socket) = launch.socket.as_deref().or(configured_socket) {
            crate::terminal_renderer::stop_and_release_terminal(repo, socket, &launch.terminal_id)
                .await?;
        }
    }
    if !unresolved.is_empty()
        && unresolved
            .iter()
            .all(|launch| launch.socket.as_deref().or(configured_socket).is_some())
    {
        return Ok(());
    }
    match unresolved.first() {
        Some(launch) => Err(launch.error()),
        None => Ok(()),
    }
}
