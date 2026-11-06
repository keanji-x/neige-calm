use calm_session::control::ControlReply;
use calm_session::terminal_session::Effect;
use calm_session::{DaemonMsg, read_frame};
use tokio::net::UnixStream;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use super::{
    OutputWake, SharedExitState, SharedRenderPlane, SupervisorControl, TerminalExitInfo,
    record_output, wake_projector,
};
use crate::db::RouteRepo;
use crate::session_projection_repo::WorkerSessionState;
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
    repo: Option<Arc<dyn RouteRepo>>,
    terminal_id: String,
    task_hook: Option<Arc<crate::scheduler::TerminalTaskHook>>,
    // Flipped only after every required exit write and callback succeeds. Teardown waits on
    // this, never on task completion: a failed write or read error proves no persisted exit.
    exit_persisted_tx: watch::Sender<bool>,
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
                    // Only the typed Store projection may write a typed terminal's exit.
                    if let Some(repo) = repo.as_ref() {
                        match repo.terminal_get(&terminal_id).await {
                            Ok(Some(term)) => match term.is_typed(repo.as_ref()).await {
                                Ok(false) => {}
                                Ok(true) | Err(_) => break,
                            },
                            Err(_) => break,
                            Ok(None) => {}
                        }
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
                    let exit_code = if signalled { None } else { status };
                    let output = output_capture
                        .lock()
                        .map(|capture| capture.snapshot())
                        .unwrap_or_else(|_| {
                            crate::terminal_renderer::output_capture::TerminalOutputSnapshot {
                                text: String::new(),
                                truncated: true,
                            }
                        });
                    let mut exit_persisted = true;
                    if let Some(repo) = repo.as_ref()
                        && let Err(e) = repo
                            .terminal_set_exit_with_output(
                                &terminal_id,
                                exit_code,
                                signalled,
                                &output.text,
                                output.truncated,
                            )
                            .await
                    {
                        exit_persisted = false;
                        tracing::warn!(
                            terminal_id = %terminal_id,
                            exit_code = ?exit_code,
                            signal_killed = signalled,
                            error = %e,
                            "failed to persist terminal exit from supervisor"
                        );
                    }
                    if let Some(repo) = repo.as_ref() {
                        let terminal_status = if signalled {
                            WorkerSessionState::Failed
                        } else {
                            WorkerSessionState::Exited
                        };
                        if let Err(e) =
                            crate::terminal_sweeper::complete_ephemeral_session_from_terminal_exit(
                                repo.as_ref(),
                                &terminal_id,
                                terminal_status,
                            )
                            .await
                        {
                            exit_persisted = false;
                            tracing::warn!(
                                terminal_id = %terminal_id,
                                exit_code = ?exit_code,
                                signal_killed = signalled,
                                error = %e,
                                "failed to complete runtime from supervisor terminal exit"
                            );
                        }
                    }
                    // Keep attempting callbacks after a failure; their owners retain completion
                    // policy and retry responsibility. A later success cannot erase an earlier error.
                    if let Some(hook) = task_hook.as_ref()
                        && let Err(e) = hook
                            .on_terminal_exit(
                                &terminal_id,
                                exit_code,
                                signalled,
                                &output.text,
                                output.truncated,
                            )
                            .await
                    {
                        exit_persisted = false;
                        tracing::warn!(terminal_id = %terminal_id, error = %e,
                            "terminal task hook failed; exit persistence remains unverified");
                    }
                    // Wake after all persistence attempts, including failures. Keep no `.await`
                    // between this wake and the success signal so teardown cannot skip the wake.
                    wake_projector(&output_wake, &terminal_id);
                    if exit_persisted {
                        let _ = exit_persisted_tx.send(true);
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
