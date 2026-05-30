use calm_session::control::ControlReply;
use calm_session::terminal_session::Effect;
use calm_session::{DaemonMsg, read_frame};
use tokio::net::UnixStream;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;

use super::{SharedExitState, SharedRenderPlane, SupervisorControl, TerminalExitInfo};
use crate::db::RouteRepo;
use crate::terminal_renderer::client_pump::apply_broadcaster_effects;
use std::sync::Arc;

// Copied from crates/calm-session/src/bin/daemon.rs::spawn_supervisor_attach_reader as part of #388 Phase 3a lift. Daemon binary retires in 3c; until then we live with duplication.
#[allow(clippy::too_many_arguments)]
pub fn spawn_supervisor_attach_reader(
    mut attach_conn: UnixStream,
    proc_id: String,
    render_plane: SharedRenderPlane,
    exit: SharedExitState,
    event_tx: broadcast::Sender<DaemonMsg>,
    supervisor_tx: mpsc::UnboundedSender<SupervisorControl>,
    exited_tx: oneshot::Sender<Option<i32>>,
    repo: Option<Arc<dyn RouteRepo>>,
    terminal_id: String,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut exited_tx = Some(exited_tx);
        loop {
            match read_frame::<ControlReply, _>(&mut attach_conn).await {
                Ok(ControlReply::Output { bytes, .. }) => {
                    let effects = match render_plane.lock() {
                        Ok(mut rp) => rp.on_pty_chunk(bytes),
                        Err(_) => Vec::new(),
                    };
                    apply_broadcaster_effects(&event_tx, &supervisor_tx, effects);
                }
                Ok(ControlReply::Exited {
                    status, signalled, ..
                }) => {
                    let effects = match render_plane.lock() {
                        Ok(mut rp) => {
                            let effects = rp.on_child_exit(status);
                            let exit_info = TerminalExitInfo {
                                code: status,
                                pty_seq: rp.pty_seq(),
                                render_rev: rp.render_rev(),
                            };
                            if let Ok(mut guard) = exit.lock()
                                && guard.is_none()
                            {
                                *guard = Some(exit_info);
                            }
                            effects
                        }
                        Err(_) => Vec::new(),
                    };
                    for eff in effects {
                        if let Effect::Broadcast(msg) = eff {
                            let _ = event_tx.send(msg);
                        }
                    }
                    if let Some(tx) = exited_tx.take() {
                        let _ = tx.send(status);
                    }
                    let exit_code = if signalled { None } else { status };
                    if let Some(repo) = repo.as_ref()
                        && let Err(e) = repo
                            .terminal_set_exit(&terminal_id, exit_code, signalled)
                            .await
                    {
                        tracing::warn!(
                            terminal_id = %terminal_id,
                            exit_code = ?exit_code,
                            signal_killed = signalled,
                            error = %e,
                            "failed to persist terminal exit from supervisor"
                        );
                    }
                    break;
                }
                Ok(ControlReply::Gap {
                    earliest_cursor,
                    requested_cursor,
                }) => {
                    tracing::warn!(
                        proc_id = %proc_id,
                        earliest_cursor,
                        requested_cursor,
                        "supervisor byte stream reported a replay gap"
                    );
                }
                Ok(other) => {
                    tracing::warn!(proc_id = %proc_id, reply = ?other, "unexpected supervisor attach frame");
                }
                Err(e) => {
                    tracing::warn!(proc_id = %proc_id, error = %e, "supervisor attach stream ended");
                    break;
                }
            }
        }
    })
}
