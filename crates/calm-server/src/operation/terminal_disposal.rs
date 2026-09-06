//! Destruction must preserve an unresolved prepared launch, even when a probe
//! currently finds no process. The original request may still be in flight.
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
    // Column is selected exclusively from the static scope above. Include
    // prepared ownership even if the terminal row was already lost historically.
    let sql = format!(
        "SELECT o.* FROM operations o JOIN cards c ON o.target_type='card' AND o.target_id=c.id JOIN tracks t ON t.id=c.track_id WHERE o.kind IN ('terminal-worker','claude-worker','codex-worker') AND {column}=?1 ORDER BY o.id"
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
            // Old successful operations predate this checkpoint. They already
            // completed the original ownership transfer; preserve their normal
            // deletion contract. Missing state at SpawnStarted is NOT success.
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

/// Transaction recheck immediately before removing rows or replacing workspace
/// ownership. Callers hold the normal OperationRuntime/Track deletion fences
/// across the external teardown; no supervisor I/O is performed in this writer.
pub(crate) async fn require_safe_tx(tx: &mut Tx<'_>, scope: &Scope) -> Result<()> {
    if let Some(unresolved) = unresolved_tx(tx, scope).await?.first() {
        return Err(unresolved.error());
    }
    Ok(())
}

/// Inspect the entire affected scope before touching any member's resources.
/// Request an exact best-effort stop, but keep the durable obligation regardless
/// of reply, missing PID, renderer state or a truthful Probe(false).
pub(crate) async fn require_safe(
    repo: &dyn RouteRepo,
    scope: Scope,
    configured_socket: Option<&Path>,
) -> Result<()> {
    let unresolved = write_in_tx_typed(repo, move |tx| {
        Box::pin(async move { unresolved_tx(tx, &scope).await })
    })
    .await?;
    for launch in &unresolved {
        if let Some(socket) = launch.socket.as_deref().or(configured_socket) {
            crate::terminal_renderer::request_terminal_stop(socket, &launch.terminal_id).await;
        }
    }
    match unresolved.first() {
        Some(launch) => Err(launch.error()),
        None => Ok(()),
    }
}
