use calm_session::control::{
    ControlMsg, ControlReply, ResizePtyRequest, SignalRequest, WriteStdinRequest,
};
use calm_session::{DaemonMsg, read_frame, write_frame};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::{PtyWrite, SupervisorControl};

// Copied from crates/calm-session/src/bin/daemon.rs::spawn_supervisor_control_writer as part of #388 Phase 3a lift. Daemon binary retires in 3c; until then we live with duplication.
pub fn spawn_supervisor_control_writer(
    mut control_conn: UnixStream,
    proc_id: String,
    mut rx: mpsc::UnboundedReceiver<SupervisorControl>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(item) = rx.recv().await {
            match item {
                SupervisorControl::Write(write) => {
                    let PtyWrite {
                        data,
                        input_seq,
                        ack,
                    } = write;
                    let write_seq = (input_seq > 0).then_some(input_seq);
                    if let Err(e) = write_frame(
                        &mut control_conn,
                        &ControlMsg::WriteStdin(WriteStdinRequest {
                            proc_id: proc_id.clone(),
                            bytes: data,
                            write_seq,
                        }),
                    )
                    .await
                    {
                        tracing::warn!(error = %e, "failed to send supervisor WriteStdin");
                        break;
                    }
                    if let Some(expected) = write_seq {
                        match read_frame::<ControlReply, _>(&mut control_conn).await {
                            Ok(ControlReply::WriteAck { write_seq }) if write_seq == expected => {
                                if let Some(ack) = ack {
                                    let _ = ack.send(DaemonMsg::InputAck {
                                        input_seq: write_seq,
                                    });
                                }
                            }
                            Ok(other) => {
                                tracing::warn!(reply = ?other, "unexpected supervisor write reply");
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "failed to read supervisor WriteAck");
                                break;
                            }
                        }
                    }
                }
                SupervisorControl::Resize { cols, rows } => {
                    if cols == 0 || rows == 0 {
                        continue;
                    }
                    if let Err(e) = write_frame(
                        &mut control_conn,
                        &ControlMsg::ResizePty(ResizePtyRequest {
                            proc_id: proc_id.clone(),
                            cols,
                            rows,
                            pixel_w: 0,
                            pixel_h: 0,
                        }),
                    )
                    .await
                    {
                        tracing::warn!(error = %e, "failed to send supervisor ResizePty");
                        break;
                    }
                    match read_frame::<ControlReply, _>(&mut control_conn).await {
                        Ok(ControlReply::ResizeOk) => {}
                        Ok(other) => {
                            tracing::warn!(reply = ?other, "unexpected supervisor resize reply")
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "failed to read supervisor ResizeOk");
                            break;
                        }
                    }
                }
                SupervisorControl::Signal(sig) => {
                    if let Err(e) = write_frame(
                        &mut control_conn,
                        &ControlMsg::Signal(SignalRequest {
                            proc_id: proc_id.clone(),
                            sig,
                        }),
                    )
                    .await
                    {
                        tracing::warn!(error = %e, "failed to send supervisor Signal");
                        break;
                    }
                    match read_frame::<ControlReply, _>(&mut control_conn).await {
                        Ok(ControlReply::SignalOk) => {}
                        Ok(other) => {
                            tracing::warn!(reply = ?other, "unexpected supervisor signal reply")
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "failed to read supervisor SignalOk");
                            break;
                        }
                    }
                }
            }
        }
    })
}
