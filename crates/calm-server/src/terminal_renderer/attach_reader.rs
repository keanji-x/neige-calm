use calm_session::control::ControlReply;
use calm_session::terminal_session::Effect;
use calm_session::{DaemonMsg, read_frame};
use tokio::net::UnixStream;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;

use super::{
    OutputWake, SharedExitState, SharedRenderPlane, SupervisorControl, TerminalExitInfo,
    record_output,
};
use crate::terminal_renderer::client_pump::apply_broadcaster_effects;
use crate::terminal_renderer::output_capture::SharedTerminalOutputCapture;
use std::sync::Arc;
use std::sync::atomic::AtomicI64;

pub(super) struct ObservationSource {
    pub(super) plane: SharedRenderPlane,
    pub(super) exited: bool,
}
impl Drop for ObservationSource {
    fn drop(&mut self) {
        if !self.exited
            && let Ok(mut plane) = self.plane.lock()
        {
            plane.invalidate_observation("terminal output source disconnected");
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn spawn_supervisor_attach_reader(
    mut attach_conn: UnixStream,
    proc_id: String,
    render_plane: SharedRenderPlane,
    exit: SharedExitState,
    event_tx: broadcast::Sender<DaemonMsg>,
    supervisor_tx: mpsc::UnboundedSender<SupervisorControl>,
    exited_tx: oneshot::Sender<Option<i32>>,
    terminal_id: String,
    completion: Arc<super::legacy_completion::LegacyCompletion>,
    output_capture: SharedTerminalOutputCapture,
    // Stamped `now_ms()` on every `Output` frame; a leading edge (a frame after a window of
    // quiet — the first frame included unless a fresh launch's replay stamped just before it) and
    // the persisted exit wake the activity projector.
    last_output_ms: Arc<AtomicI64>,
    output_wake: OutputWake,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut source = ObservationSource {
            plane: render_plane.clone(),
            exited: false,
        };
        let mut exited_tx = Some(exited_tx);
        loop {
            match read_frame::<ControlReply, _>(&mut attach_conn).await {
                Ok(ControlReply::Output { bytes, .. }) => {
                    // One atomic swap: the previous stamp decides whether
                    // this frame is a leading edge (quiet → output); there
                    // is no trailing edge — output → quiet is the tick's.
                    record_output(&last_output_ms, &output_wake, &terminal_id);
                    if let Ok(mut capture) = output_capture.lock() {
                        capture.push(&bytes);
                    }
                    let effects = match render_plane.lock() {
                        Ok(mut rp) => rp.on_pty_chunk(bytes),
                        Err(_) => Vec::new(),
                    };
                    apply_broadcaster_effects(&event_tx, &supervisor_tx, effects);
                }
                Ok(ControlReply::Exited {
                    status, signalled, ..
                }) => {
                    // Preserve the transport fact before classification or any other I/O.
                    // Only completion's current ownership check grants projection authority.
                    let output = output_capture
                        .lock()
                        .map(|capture| capture.snapshot())
                        .unwrap_or_else(|_| {
                            crate::terminal_renderer::output_capture::TerminalOutputSnapshot {
                                text: String::new(),
                                truncated: true,
                            }
                        });
                    if let Err(error) = completion.record(super::legacy_completion::LegacyExit {
                        code: if signalled { None } else { status },
                        signalled,
                        output,
                    }) {
                        tracing::warn!(%terminal_id, %error, "cannot record legacy exit");
                        break;
                    }
                    source.exited = true;
                    let effects = match render_plane.lock() {
                        Ok(mut rp) => {
                            let effects = rp.on_child_exit(status);
                            let exit_info = TerminalExitInfo {
                                code: status,
                                pty_seq: rp.pty_seq(),
                                render_rev: rp.render_rev(),
                                exited_at: std::time::SystemTime::now(),
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
                    if let Err(error) = completion.retry().await {
                        tracing::warn!(terminal_id = %terminal_id, %error,
                            "legacy completion failed; retained observation can be retried");
                    }
                    break;
                }
                Ok(ControlReply::Gap {
                    earliest_cursor,
                    requested_cursor,
                }) => {
                    if let Ok(mut plane) = render_plane.lock() {
                        plane.invalidate_observation("terminal output gap");
                    }
                    if let Ok(mut capture) = output_capture.lock() {
                        capture.mark_gap();
                    }
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
