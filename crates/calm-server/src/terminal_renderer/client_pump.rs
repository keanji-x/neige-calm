use std::time::Duration;

use calm_session::terminal_model::ScrollbackLimit;
use calm_session::terminal_session::{Effect, SessionContext, TerminalSessionState};
use calm_session::{ClientMsg, DaemonMsg};
use tokio::sync::{broadcast, mpsc};
use uuid::Uuid;

use super::{PtyWrite, SharedExitState, SharedOwnerRegistry, SharedRenderPlane, SupervisorControl};
use crate::terminal_renderer::snapshot::{rebuild_server_hello_snapshot, scrollback_request};

pub struct ClientPumpContext {
    pub event_rx: broadcast::Receiver<DaemonMsg>,
    pub event_tx: broadcast::Sender<DaemonMsg>,
    pub render_plane: SharedRenderPlane,
    pub exit: SharedExitState,
    pub supervisor_tx: mpsc::UnboundedSender<SupervisorControl>,
    pub owner_registry: SharedOwnerRegistry,
    pub session_id: Uuid,
    pub terminal_id: String,
}

pub async fn run_client_pump(
    mut incoming_rx: mpsc::Receiver<ClientMsg>,
    outgoing_tx: mpsc::Sender<DaemonMsg>,
    ctx: ClientPumpContext,
) -> anyhow::Result<()> {
    let ClientPumpContext {
        event_rx,
        event_tx,
        render_plane,
        exit,
        supervisor_tx,
        owner_registry,
        session_id,
        terminal_id,
    } = ctx;
    let mut state = TerminalSessionState::new();
    let (per_client_tx, mut per_client_rx) = mpsc::unbounded_channel::<DaemonMsg>();

    let first = match incoming_rx.recv().await {
        Some(first) => first,
        None => return Ok(()),
    };
    let scrollback_request = match &first {
        ClientMsg::ClientHello {
            initial_scrollback, ..
        } => Some(scrollback_request(*initial_scrollback)),
        _ => None,
    };

    let first_effects = {
        let guard = render_plane.lock().unwrap();
        let mut reg = owner_registry.lock().unwrap();
        let ctx = SessionContext {
            terminal_id: &terminal_id,
            session_id,
            pty_size: guard.current_size(),
            pty_seq_head: guard.pty_seq_head(),
            pty_seq_tail: guard.pty_seq(),
            render_rev: guard.render_rev(),
            is_child_ready: guard.child_ready_fired(),
            current_default_fg: guard.default_fg(),
            current_default_bg: guard.default_bg(),
        };
        state.on_client_frame(first, guard.transcript(), &mut reg, &ctx)
    };

    let mut handshake_failed = false;
    for eff in first_effects {
        match eff {
            Effect::ResizePty { cols, rows } => {
                request_resize(&supervisor_tx, cols, rows);
                if let Ok(mut rp) = render_plane.lock() {
                    let _ = rp.on_resize(cols, rows);
                }
            }
            Effect::SendToClient(msg) => {
                let msg = rebuild_server_hello_snapshot(msg, &render_plane, scrollback_request);
                let sent_server_hello = matches!(msg, DaemonMsg::ServerHello { .. });
                if outgoing_tx.send(msg).await.is_err() {
                    return Ok(());
                }
                if sent_server_hello {
                    let exit_info = exit.lock().ok().and_then(|guard| guard.clone());
                    if let Some(exit_info) = exit_info {
                        let exited = DaemonMsg::TerminalExited {
                            code: exit_info.code,
                            pty_seq: exit_info.pty_seq,
                            render_rev: exit_info.render_rev,
                        };
                        if outgoing_tx.send(exited).await.is_err() {
                            return Ok(());
                        }
                    }
                }
            }
            Effect::SendProtocolError {
                code,
                message,
                expected_version,
            } => {
                let _ = outgoing_tx
                    .send(DaemonMsg::ProtocolError {
                        code,
                        message,
                        expected_version,
                    })
                    .await;
                handshake_failed = true;
            }
            Effect::CloseConnection => {
                handshake_failed = true;
            }
            Effect::ProtocolViolation(reason) => {
                anyhow::bail!("{reason}");
            }
            Effect::Broadcast(_)
            | Effect::WriteToPty { .. }
            | Effect::KillChild
            | Effect::AssignOwner(_)
            | Effect::BroadcastOwnerChanged(_)
            | Effect::TerminalThemeUpdate { .. } => {
                tracing::warn!("unexpected effect on first frame; ignoring");
            }
        }
    }
    if handshake_failed {
        return Ok(());
    }

    let down_render_plane = render_plane.clone();
    let mut down_event_rx = event_rx;
    let down_outgoing_tx = outgoing_tx.clone();
    let down_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                broadcast = down_event_rx.recv() => match broadcast {
                    Ok(msg) => {
                        let is_exit = matches!(msg, DaemonMsg::TerminalExited { .. });
                        if down_outgoing_tx.send(msg).await.is_err() {
                            break;
                        }
                        if is_exit {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(lagged = n, "client lagged; sending SnapshotRequired + fresh snapshot");
                        let snap = {
                            let rp = down_render_plane.lock().unwrap();
                            let sz = rp.current_size();
                            rp.build_snapshot(
                                sz.cols,
                                sz.rows,
                                scrollback_request.unwrap_or(ScrollbackLimit::None),
                            )
                        };
                        let required = DaemonMsg::SnapshotRequired {
                            reason: format!("broadcast lagged by {n} frames"),
                        };
                        if down_outgoing_tx.send(required).await.is_err() {
                            break;
                        }
                        if down_outgoing_tx
                            .send(DaemonMsg::RenderSnapshot(snap))
                            .await
                            .is_err()
                        {
                            break;
                        }
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                direct = per_client_rx.recv() => match direct {
                    Some(msg) => {
                        let is_exit = matches!(msg, DaemonMsg::TerminalExited { .. });
                        if down_outgoing_tx.send(msg).await.is_err() {
                            break;
                        }
                        if is_exit {
                            break;
                        }
                    }
                    None => break,
                }
            }
        }
    });

    let client_id_for_disconnect = state.client_id();
    loop {
        let Some(msg) = incoming_rx.recv().await else {
            break;
        };
        let effects = {
            let guard = render_plane.lock().unwrap();
            let mut reg = owner_registry.lock().unwrap();
            let ctx = SessionContext {
                terminal_id: &terminal_id,
                session_id,
                pty_size: guard.current_size(),
                pty_seq_head: guard.pty_seq_head(),
                pty_seq_tail: guard.pty_seq(),
                render_rev: guard.render_rev(),
                is_child_ready: guard.child_ready_fired(),
                current_default_fg: guard.default_fg(),
                current_default_bg: guard.default_bg(),
            };
            state.on_client_frame(msg, guard.transcript(), &mut reg, &ctx)
        };

        let mut closed = false;
        for eff in effects {
            match eff {
                Effect::SendToClient(msg) => {
                    let _ = per_client_tx.send(msg);
                }
                Effect::Broadcast(msg) => {
                    let _ = event_tx.send(msg);
                }
                Effect::ResizePty { cols, rows } => {
                    request_resize(&supervisor_tx, cols, rows);
                    if let Ok(mut rp) = render_plane.lock() {
                        let resize_effects = rp.on_resize(cols, rows);
                        for re in resize_effects {
                            if let Effect::Broadcast(m) = re {
                                let _ = event_tx.send(m);
                            }
                        }
                    }
                }
                Effect::WriteToPty { data, input_seq } => {
                    let ack = if input_seq > 0 {
                        Some(per_client_tx.clone())
                    } else {
                        None
                    };
                    if supervisor_tx
                        .send(SupervisorControl::Write(PtyWrite {
                            data,
                            input_seq,
                            ack,
                        }))
                        .is_err()
                    {
                        closed = true;
                        break;
                    }
                }
                Effect::KillChild => {
                    tracing::info!("client requested Kill; signaling child");
                    signal_child(&supervisor_tx);
                }
                Effect::SendProtocolError {
                    code,
                    message,
                    expected_version,
                } => {
                    let _ = per_client_tx.send(DaemonMsg::ProtocolError {
                        code,
                        message,
                        expected_version,
                    });
                }
                Effect::CloseConnection => {
                    closed = true;
                    break;
                }
                Effect::AssignOwner(_) => {}
                Effect::BroadcastOwnerChanged(owner) => {
                    let _ = event_tx.send(DaemonMsg::OwnerChanged {
                        owner_client_id: owner,
                    });
                }
                Effect::ProtocolViolation(reason) => {
                    down_task.abort();
                    let _ = down_task.await;
                    anyhow::bail!("{reason}");
                }
                Effect::TerminalThemeUpdate { fg, bg } => {
                    let focus_event_tracking = if let Ok(mut rp) = render_plane.lock() {
                        rp.set_default_colors(Some(fg), Some(bg));
                        rp.focus_event_tracking()
                    } else {
                        false
                    };
                    if !focus_event_tracking {
                        continue;
                    }
                    if supervisor_tx
                        .send(SupervisorControl::Write(PtyWrite {
                            data: b"\x1b[I".to_vec(),
                            input_seq: 0,
                            ack: None,
                        }))
                        .is_err()
                    {
                        closed = true;
                        break;
                    }
                }
            }
        }
        if closed {
            break;
        }
    }

    if let Some(cid) = client_id_for_disconnect {
        let changed = {
            let mut reg = owner_registry.lock().unwrap();
            reg.on_release(cid)
        };
        if changed {
            let _ = event_tx.send(DaemonMsg::OwnerChanged {
                owner_client_id: None,
            });
        }
    }

    drop(per_client_tx);
    down_task.abort();
    let _ = down_task.await;
    Ok(())
}

// Copied from crates/calm-session/src/bin/daemon.rs::apply_broadcaster_effects as part of #388 Phase 3a lift. Daemon binary retires in 3c; until then we live with duplication.
pub(crate) fn apply_broadcaster_effects(
    tx: &broadcast::Sender<DaemonMsg>,
    supervisor_tx: &mpsc::UnboundedSender<SupervisorControl>,
    effects: Vec<Effect>,
) {
    for eff in effects {
        match eff {
            Effect::Broadcast(msg) => {
                let _ = tx.send(msg);
            }
            Effect::WriteToPty { data, input_seq } => {
                let _ = supervisor_tx.send(SupervisorControl::Write(PtyWrite {
                    data,
                    input_seq,
                    ack: None,
                }));
            }
            Effect::SendToClient(_)
            | Effect::ResizePty { .. }
            | Effect::KillChild
            | Effect::SendProtocolError { .. }
            | Effect::CloseConnection
            | Effect::AssignOwner(_)
            | Effect::BroadcastOwnerChanged(_)
            | Effect::ProtocolViolation(_)
            | Effect::TerminalThemeUpdate { .. } => {
                tracing::warn!(
                    "RenderPlane emitted non-Broadcast non-WriteToPty effect; dropping (this is a bug)"
                );
            }
        }
    }
}

// Copied from crates/calm-session/src/bin/daemon.rs::request_resize as part of #388 Phase 3a lift. Daemon binary retires in 3c; until then we live with duplication.
fn request_resize(supervisor_tx: &mpsc::UnboundedSender<SupervisorControl>, cols: u16, rows: u16) {
    if cols == 0 || rows == 0 {
        return;
    }
    let _ = supervisor_tx.send(SupervisorControl::Resize { cols, rows });
}

// Copied from crates/calm-session/src/bin/daemon.rs::signal_child as part of #388 Phase 3a lift. Daemon binary retires in 3c; until then we live with duplication.
fn signal_child(supervisor_tx: &mpsc::UnboundedSender<SupervisorControl>) {
    let _ = supervisor_tx.send(SupervisorControl::Signal(
        calm_session::control::ProcSignal::Hup,
    ));
    let supervisor_tx = supervisor_tx.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let _ = supervisor_tx.send(SupervisorControl::Signal(
            calm_session::control::ProcSignal::Kill,
        ));
    });
}
